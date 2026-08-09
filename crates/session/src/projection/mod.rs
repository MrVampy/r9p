mod handler;
#[cfg(test)]
mod tests;

use crate::{parse_namespace_path, Client, ConnectionConfig, Error, Result};
use handler::ProjectionHandler;
use r9p::{
    codec::Variant,
    server::{serve_connection, ServerConfig},
};
use std::{
    collections::BTreeMap,
    fs, io,
    os::unix::{
        fs::{FileTypeExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Condvar, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

const MAX_SESSIONS: usize = 256;
const MAX_ASYNC_REQUESTS: usize = 1024;

/// A private local projection of one authenticated namespace subtree.
///
/// Each local 9P connection receives its own upstream namespace client. The
/// upstream client follows 9P2000.R referrals internally, while the local
/// projection exposes only the selected subtree and no referrals. A child can
/// therefore use the namespace without receiving the upstream credential.
#[derive(Clone)]
pub struct NamespaceProjectionConfig {
    pub socket: PathBuf,
    pub namespace: ConnectionConfig,
    pub source: String,
    pub max_sessions: usize,
    pub max_async_requests: usize,
    pub connect_timeout: Duration,
    pub operation_timeout: Duration,
}

pub struct NamespaceProjection {
    socket: PathBuf,
    counters: Arc<ProjectionCounters>,
    sessions: Arc<ActiveSessions>,
    shutdown: Arc<AtomicBool>,
    acceptor: Option<JoinHandle<()>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NamespaceProjectionStatus {
    pub active_sessions: usize,
    pub accepted_sessions: u64,
    pub rejected_sessions: u64,
    pub connection_failures: u64,
    pub completed_sessions: u64,
}

#[derive(Default)]
struct ProjectionCounters {
    active_sessions: AtomicUsize,
    accepted_sessions: AtomicU64,
    rejected_sessions: AtomicU64,
    connection_failures: AtomicU64,
    completed_sessions: AtomicU64,
}

#[derive(Default)]
struct ActiveSessions {
    state: Mutex<ActiveState>,
    idle: Condvar,
}

#[derive(Default)]
struct ActiveState {
    next_id: u64,
    active: usize,
    clients: BTreeMap<u64, Client>,
    local_streams: BTreeMap<u64, UnixStream>,
}

impl NamespaceProjection {
    pub fn start(config: NamespaceProjectionConfig) -> Result<Self> {
        validate_config(&config)?;
        prepare_socket(&config.socket)?;
        let listener = UnixListener::bind(&config.socket)
            .map_err(|error| Error::io("bind namespace projection", error))?;
        fs::set_permissions(&config.socket, fs::Permissions::from_mode(0o600))
            .map_err(|error| Error::io("protect namespace projection socket", error))?;

        let counters = Arc::new(ProjectionCounters::default());
        let sessions = Arc::new(ActiveSessions::default());
        let shutdown = Arc::new(AtomicBool::new(false));
        let acceptor = spawn_acceptor(
            listener,
            config.clone(),
            Arc::clone(&counters),
            Arc::clone(&sessions),
            Arc::clone(&shutdown),
        )?;
        Ok(Self {
            socket: config.socket,
            counters,
            sessions,
            shutdown,
            acceptor: Some(acceptor),
        })
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    pub fn status(&self) -> NamespaceProjectionStatus {
        NamespaceProjectionStatus {
            active_sessions: self.counters.active_sessions.load(Ordering::Acquire),
            accepted_sessions: self.counters.accepted_sessions.load(Ordering::Acquire),
            rejected_sessions: self.counters.rejected_sessions.load(Ordering::Acquire),
            connection_failures: self.counters.connection_failures.load(Ordering::Acquire),
            completed_sessions: self.counters.completed_sessions.load(Ordering::Acquire),
        }
    }
}

impl Drop for NamespaceProjection {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        self.sessions.shutdown_clients();
        let _ = UnixStream::connect(&self.socket);
        if let Some(acceptor) = self.acceptor.take() {
            let _ = acceptor.join();
        }
        self.sessions.wait_until_idle();
        remove_socket(&self.socket);
    }
}

fn validate_config(config: &NamespaceProjectionConfig) -> Result<()> {
    let source_is_valid = (config.source == "/"
        || (config.source.starts_with('/') && !config.source.ends_with('/')))
        && parse_namespace_path(config.source.as_bytes()).is_ok();
    if !config.socket.is_absolute()
        || config.socket.file_name().is_none()
        || !source_is_valid
        || config.max_sessions == 0
        || config.max_sessions > MAX_SESSIONS
        || config.max_async_requests == 0
        || config.max_async_requests > MAX_ASYNC_REQUESTS
        || config.connect_timeout.is_zero()
        || config.operation_timeout.is_zero()
    {
        return Err(Error::new(
            libc::EINVAL,
            "namespace projection requires an absolute socket, a canonical source, bounded concurrency, and finite timeouts",
        ));
    }
    Ok(())
}

fn prepare_socket(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => fs::remove_file(path)
            .map_err(|error| Error::io("remove stale namespace projection socket", error)),
        Ok(_) => Err(Error::new(
            libc::EEXIST,
            format!(
                "namespace projection path already exists: {}",
                path.display()
            ),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::io("inspect namespace projection socket", error)),
    }
}

fn remove_socket(path: &Path) {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket()) {
        let _ = fs::remove_file(path);
    }
}

fn spawn_acceptor(
    listener: UnixListener,
    config: NamespaceProjectionConfig,
    counters: Arc<ProjectionCounters>,
    sessions: Arc<ActiveSessions>,
    shutdown: Arc<AtomicBool>,
) -> Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("r9p-namespace-projection".to_string())
        .spawn(move || {
            while !shutdown.load(Ordering::Acquire) {
                let local = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                };
                if shutdown.load(Ordering::Acquire) {
                    let _ = local.shutdown(std::net::Shutdown::Both);
                    break;
                }
                let Some(slot) = SessionSlot::try_acquire(
                    Arc::clone(&sessions),
                    Arc::clone(&counters),
                    config.max_sessions,
                ) else {
                    counters.rejected_sessions.fetch_add(1, Ordering::AcqRel);
                    let _ = local.shutdown(std::net::Shutdown::Both);
                    continue;
                };
                counters.accepted_sessions.fetch_add(1, Ordering::AcqRel);
                let worker_config = config.clone();
                let worker_counters = Arc::clone(&counters);
                let worker_shutdown = Arc::clone(&shutdown);
                if thread::Builder::new()
                    .name("r9p-namespace-projection-session".to_string())
                    .spawn(move || {
                        serve_session(local, worker_config, worker_counters, worker_shutdown, slot);
                    })
                    .is_err()
                {
                    counters.rejected_sessions.fetch_add(1, Ordering::AcqRel);
                }
            }
        })
        .map_err(|error| Error::new(libc::EIO, format!("spawn projection acceptor: {error}")))
}

fn serve_session(
    local: UnixStream,
    config: NamespaceProjectionConfig,
    counters: Arc<ProjectionCounters>,
    shutdown: Arc<AtomicBool>,
    mut slot: SessionSlot,
) {
    let client = match Client::connect_with_timeout(&config.namespace, config.connect_timeout) {
        Ok(client) => client,
        Err(_) => {
            counters.connection_failures.fetch_add(1, Ordering::AcqRel);
            let _ = local.shutdown(std::net::Shutdown::Both);
            return;
        }
    };
    if shutdown.load(Ordering::Acquire) {
        let _ = client.shutdown();
        let _ = local.shutdown(std::net::Shutdown::Both);
        return;
    }
    if !slot.register(client.clone(), &local, &shutdown) {
        let _ = client.shutdown();
        let _ = local.shutdown(std::net::Shutdown::Both);
        return;
    }
    let handler = Arc::new(ProjectionHandler::new(
        client.clone(),
        config.source,
        config.operation_timeout,
    ));
    let server = ServerConfig {
        default_msize: config.namespace.msize,
        max_msize: config.namespace.msize,
        max_async_requests: config.max_async_requests,
        variant: Variant::R,
        session_uname: None,
        ..ServerConfig::default()
    };
    let _ = serve_connection(local, server, handler);
    let _ = client.shutdown();
    counters.completed_sessions.fetch_add(1, Ordering::AcqRel);
}

impl ActiveSessions {
    fn acquire(self: &Arc<Self>, limit: usize) -> Option<SessionSlot> {
        let mut state = self.state.lock().ok()?;
        if state.active >= limit {
            return None;
        }
        let id = state.next_id;
        state.next_id = state.next_id.wrapping_add(1);
        state.active += 1;
        Some(SessionSlot {
            sessions: Arc::clone(self),
            counters: None,
            id,
            registered: false,
        })
    }

    fn shutdown_clients(&self) {
        let (clients, local_streams) = self
            .state
            .lock()
            .map(|state| {
                (
                    state.clients.values().cloned().collect::<Vec<_>>(),
                    state
                        .local_streams
                        .values()
                        .filter_map(|stream| stream.try_clone().ok())
                        .collect::<Vec<_>>(),
                )
            })
            .unwrap_or_default();
        for client in clients {
            let _ = client.shutdown();
        }
        for stream in local_streams {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }

    fn wait_until_idle(&self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        while state.active != 0 {
            state = match self.idle.wait(state) {
                Ok(state) => state,
                Err(error) => error.into_inner(),
            };
        }
    }
}

struct SessionSlot {
    sessions: Arc<ActiveSessions>,
    counters: Option<Arc<ProjectionCounters>>,
    id: u64,
    registered: bool,
}

impl SessionSlot {
    fn try_acquire(
        sessions: Arc<ActiveSessions>,
        counters: Arc<ProjectionCounters>,
        limit: usize,
    ) -> Option<Self> {
        let mut slot = sessions.acquire(limit)?;
        counters.active_sessions.fetch_add(1, Ordering::AcqRel);
        slot.counters = Some(counters);
        Some(slot)
    }

    fn register(&mut self, client: Client, local: &UnixStream, shutdown: &AtomicBool) -> bool {
        let Ok(local) = local.try_clone() else {
            return false;
        };
        let Ok(mut state) = self.sessions.state.lock() else {
            return false;
        };
        if shutdown.load(Ordering::Acquire) {
            return false;
        }
        state.clients.insert(self.id, client);
        state.local_streams.insert(self.id, local);
        self.registered = true;
        true
    }
}

impl Drop for SessionSlot {
    fn drop(&mut self) {
        if let Ok(mut state) = self.sessions.state.lock() {
            if self.registered {
                state.clients.remove(&self.id);
                state.local_streams.remove(&self.id);
            }
            state.active = state.active.saturating_sub(1);
            if state.active == 0 {
                self.sessions.idle.notify_all();
            }
        }
        if let Some(counters) = &self.counters {
            counters.active_sessions.fetch_sub(1, Ordering::AcqRel);
        }
    }
}
