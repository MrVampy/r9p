use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, RwLock,
    },
    time::Duration,
};

use crate::{Client, ConnectionConfig, Error, Result, SessionEpoch};

/// One renewable attachment to a logical 9P namespace.
///
/// The session owns connection establishment, serialized replacement, and the
/// epoch that identifies the current fid namespace. Consumers remain
/// responsible for deciding which operations are safe to retry and for
/// rebuilding any path, open-fid, cursor, or application state after a
/// replacement.
#[derive(Clone)]
pub struct ClientSession {
    config: Arc<ConnectionConfig>,
    connect_timeout: Duration,
    current: Arc<RwLock<Client>>,
    epoch: SessionEpoch,
    reconnect: Arc<Mutex<()>>,
    closed: Arc<AtomicBool>,
}

impl ClientSession {
    pub fn connect(config: &ConnectionConfig, connect_timeout: Duration) -> Result<Self> {
        let client = Client::connect_with_timeout(config, connect_timeout)?;
        Ok(Self {
            config: Arc::new(config.clone()),
            connect_timeout,
            current: Arc::new(RwLock::new(client)),
            epoch: SessionEpoch::new(),
            reconnect: Arc::new(Mutex::new(())),
            closed: Arc::new(AtomicBool::new(false)),
        })
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
        self.replace_from(&current)
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
        self.replace_from(&current)
    }

    pub fn session_epoch(&self) -> Result<String> {
        self.epoch.current()
    }

    pub fn epoch(&self) -> SessionEpoch {
        self.epoch.clone()
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

    fn replace_from(&self, current: &Client) -> Result<Client> {
        if self.closed.load(Ordering::Acquire) {
            return Err(Error::new(libc::ESHUTDOWN, "9P client session is closed"));
        }
        let replacement = Client::connect_with_tracker_timeout(
            &self.config,
            current.tracker(),
            self.connect_timeout,
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
        self.epoch.bump()?;
        Ok(replacement)
    }
}
