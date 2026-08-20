use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex, RwLock,
    },
    time::Duration,
};

use crate::{Client, ConnectionConfig, ConnectionSet, Error, RequestTracker, Result, SessionEpoch};

/// One established namespace attachment that can perform an initial operation
/// before becoming a renewable [`ClientSession`].
///
/// Keeping preparation and adoption in one type prevents callers from pairing
/// an arbitrary connected client with unrelated reconnect configuration.
pub struct PreparedClientSession {
    connections: ConnectionSet,
    active_candidate: usize,
    connect_timeout: Duration,
    client: Client,
}

impl PreparedClientSession {
    pub fn connect(config: &ConnectionConfig, connect_timeout: Duration) -> Result<Self> {
        Self::connect_set(&ConnectionSet::single(config.clone()), connect_timeout)
    }

    pub fn connect_set(connections: &ConnectionSet, connect_timeout: Duration) -> Result<Self> {
        let (active_candidate, client) =
            connect_from(connections, RequestTracker::default(), connect_timeout, 0)?;
        Ok(Self {
            connections: connections.clone(),
            active_candidate,
            connect_timeout,
            client,
        })
    }

    /// Returns the established attachment for caller-owned initial work.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Transfers the same authenticated attachment into renewable ownership.
    pub fn into_session(self) -> ClientSession {
        ClientSession::from_connected(
            self.connections,
            self.active_candidate,
            self.connect_timeout,
            self.client,
        )
    }
}

/// One renewable attachment to a logical 9P namespace.
///
/// The session owns connection establishment, serialized replacement, and the
/// epoch that identifies the current fid namespace. Consumers remain
/// responsible for deciding which operations are safe to retry and for
/// rebuilding any path, open-fid, cursor, or application state after a
/// replacement.
#[derive(Clone)]
pub struct ClientSession {
    connections: Arc<ConnectionSet>,
    active_candidate: Arc<AtomicUsize>,
    connect_timeout: Duration,
    current: Arc<RwLock<Client>>,
    epoch: SessionEpoch,
    reconnect: Arc<Mutex<()>>,
    closed: Arc<AtomicBool>,
}

impl ClientSession {
    pub fn connect(config: &ConnectionConfig, connect_timeout: Duration) -> Result<Self> {
        Self::connect_set(&ConnectionSet::single(config.clone()), connect_timeout)
    }

    pub fn connect_set(connections: &ConnectionSet, connect_timeout: Duration) -> Result<Self> {
        let (active_candidate, client) =
            connect_from(connections, RequestTracker::default(), connect_timeout, 0)?;
        Ok(Self::from_connected(
            connections.clone(),
            active_candidate,
            connect_timeout,
            client,
        ))
    }

    fn from_connected(
        connections: ConnectionSet,
        active_candidate: usize,
        connect_timeout: Duration,
        client: Client,
    ) -> Self {
        Self {
            connections: Arc::new(connections),
            active_candidate: Arc::new(AtomicUsize::new(active_candidate)),
            connect_timeout,
            current: Arc::new(RwLock::new(client)),
            epoch: SessionEpoch::new(),
            reconnect: Arc::new(Mutex::new(())),
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn snapshot(&self) -> Result<Client> {
        if self.closed.load(Ordering::Acquire) {
            return Err(Error::new(libc::ESHUTDOWN, "9P client session is closed"));
        }
        self.current
            .read()
            .map_err(|_| Error::new(libc::EIO, "9P client session lock poisoned"))
            .map(|client| client.clone())
    }

    /// Establishes a fresh attachment and makes it current.
    ///
    /// This never replays a consumer operation. The caller must rebuild any
    /// state derived from the previous fid namespace.
    pub fn reconnect(&self) -> Result<Client> {
        let _guard = self
            .reconnect
            .lock()
            .map_err(|_| Error::new(libc::EIO, "9P client reconnect lock poisoned"))?;
        let current = self.snapshot()?;
        self.replace_from(&current, self.active_candidate.load(Ordering::Acquire))
    }

    /// Reconnects only when `failed` is still the current attachment.
    ///
    /// Concurrent callers that observed the same failed attachment share one
    /// replacement. A later caller receives the already-current client.
    pub fn reconnect_after(&self, failed: &Client) -> Result<Client> {
        let _guard = self
            .reconnect
            .lock()
            .map_err(|_| Error::new(libc::EIO, "9P client reconnect lock poisoned"))?;
        let current = self.snapshot()?;
        if !current.same_session(failed) {
            return Ok(current);
        }
        let active = self.active_candidate.load(Ordering::Acquire);
        self.replace_from(&current, (active + 1) % self.connections.candidate_count())
    }

    pub fn session_epoch(&self) -> Result<String> {
        self.epoch.current()
    }

    pub fn epoch(&self) -> SessionEpoch {
        self.epoch.clone()
    }

    pub fn active_address(&self) -> String {
        self.connections.candidates()[self.active_candidate.load(Ordering::Acquire)]
            .address
            .clone()
    }

    pub fn candidate_addresses(&self) -> Vec<String> {
        self.connections
            .candidates()
            .iter()
            .map(|candidate| candidate.address.clone())
            .collect()
    }

    /// Permanently closes this session and interrupts calls on its current
    /// attachment. A closed session cannot reconnect.
    pub fn shutdown(&self) -> Result<()> {
        self.closed.store(true, Ordering::Release);
        let client = self
            .current
            .read()
            .map_err(|_| Error::new(libc::EIO, "9P client session lock poisoned"))?
            .clone();
        client.shutdown()
    }

    fn replace_from(&self, current: &Client, start_candidate: usize) -> Result<Client> {
        if self.closed.load(Ordering::Acquire) {
            return Err(Error::new(libc::ESHUTDOWN, "9P client session is closed"));
        }
        let (active_candidate, replacement) = connect_from(
            &self.connections,
            current.tracker(),
            self.connect_timeout,
            start_candidate,
        )?;
        if self.closed.load(Ordering::Acquire) {
            let _ = replacement.shutdown();
            return Err(Error::new(libc::ESHUTDOWN, "9P client session is closed"));
        }
        {
            let mut client = self
                .current
                .write()
                .map_err(|_| Error::new(libc::EIO, "9P client session lock poisoned"))?;
            *client = replacement.clone();
        }
        self.active_candidate
            .store(active_candidate, Ordering::Release);
        self.epoch.bump()?;
        Ok(replacement)
    }
}

fn connect_from(
    connections: &ConnectionSet,
    tracker: RequestTracker,
    connect_timeout: Duration,
    start_candidate: usize,
) -> Result<(usize, Client)> {
    let candidates = connections.candidates();
    let mut failures = Vec::new();
    let mut last_errno = libc::EHOSTUNREACH;

    for offset in 0..candidates.len() {
        let index = (start_candidate + offset) % candidates.len();
        let candidate = &candidates[index];
        match Client::connect_with_tracker_timeout(candidate, tracker.clone(), connect_timeout) {
            Ok(client) => return Ok((index, client)),
            Err(error) if error.is_transient_connection_failure() => {
                last_errno = error.errno;
                failures.push(format!("{}: {}", candidate.address, error.message()));
            }
            Err(error) => {
                return Err(Error::new(
                    error.errno,
                    format!(
                        "9P connection candidate {} failed closed: {}",
                        candidate.address,
                        error.message()
                    ),
                ));
            }
        }
    }

    Err(Error::new(
        last_errno,
        format!(
            "all 9P connection candidates failed: {}",
            failures.join("; ")
        ),
    ))
}
