use std::{
    io,
    net::{TcpStream, ToSocketAddrs},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Condvar, Mutex, MutexGuard,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use fs::{LocalTree, LocalTreeConfig};
use r9p::{
    codec::Variant,
    endpoint::TcpEndpoint,
    error::{Error, Result},
    server::{
        serve_connection, serve_file_tree_connection, ConnectionHandler, FileTree, ServerConfig,
    },
};
use r9p_auth::{
    authenticate_client_to, authenticate_server, ClientConfig, SecureStream,
    ServerConfig as SessionAuthConfig,
};

use crate::{configure_transport_socket, receive_session_claim};

#[derive(Clone)]
pub struct ReverseExportConfig {
    pub broker_endpoint: TcpEndpoint,
    pub auth: ClientConfig,
    pub expected_responder: String,
    pub principal: String,
    pub connection_pool: usize,
    pub connect_timeout: Duration,
    pub authentication_timeout: Duration,
    pub reconnect_min_delay: Duration,
    pub reconnect_max_delay: Duration,
    pub server: ServerConfig,
}

#[derive(Clone)]
pub struct FilesystemExportConfig {
    pub broker_endpoint: TcpEndpoint,
    pub auth: ClientConfig,
    pub expected_responder: String,
    pub principal: String,
    pub root: PathBuf,
    pub writable: bool,
    pub connection_pool: usize,
    pub connect_timeout: Duration,
    pub authentication_timeout: Duration,
    pub reconnect_min_delay: Duration,
    pub reconnect_max_delay: Duration,
    pub msize: u32,
    pub max_fids: usize,
}

pub struct FilesystemExport {
    inner: ReverseExport,
}

pub struct ReverseExport {
    state: ExportState,
    threads: Vec<JoinHandle<()>>,
}

#[derive(Clone)]
struct ExportState {
    shutdown: Arc<AtomicBool>,
    connected_streams: Arc<AtomicUsize>,
    lifecycle: Arc<ExportLifecycle>,
    connection_failures: Arc<AtomicU64>,
    authentication_failures: Arc<AtomicU64>,
    completed_sessions: Arc<AtomicU64>,
    active_streams: Arc<Mutex<Vec<Option<r9p_auth::SecureStream>>>>,
}

#[derive(Default)]
struct ExportLifecycle {
    gate: Mutex<()>,
    changed: Condvar,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReverseExportStatus {
    pub connected_streams: usize,
    pub connection_failures: u64,
    pub authentication_failures: u64,
    pub completed_sessions: u64,
}

impl FilesystemExport {
    pub fn start(config: FilesystemExportConfig) -> Result<Self> {
        LocalTree::open_with_config(
            &config.root,
            LocalTreeConfig {
                writable: config.writable,
            },
        )?;
        let root = config.root.clone();
        let writable = config.writable;
        let inner = ReverseExport::start(
            ReverseExportConfig {
                broker_endpoint: config.broker_endpoint,
                auth: config.auth,
                expected_responder: config.expected_responder,
                principal: config.principal,
                connection_pool: config.connection_pool,
                connect_timeout: config.connect_timeout,
                authentication_timeout: config.authentication_timeout,
                reconnect_min_delay: config.reconnect_min_delay,
                reconnect_max_delay: config.reconnect_max_delay,
                server: ServerConfig {
                    default_msize: config.msize,
                    max_msize: config.msize,
                    max_fids: config.max_fids,
                    variant: Variant::R,
                    ..ServerConfig::default()
                },
            },
            move || LocalTree::open_with_config(&root, LocalTreeConfig { writable }),
        )?;
        Ok(Self { inner })
    }

    pub fn connected_streams(&self) -> usize {
        self.inner.connected_streams()
    }

    pub fn wait_for_connected(&self, timeout: Duration) -> bool {
        self.inner.wait_for_connected(timeout)
    }

    pub fn status(&self) -> ReverseExportStatus {
        self.inner.status()
    }
}

impl ReverseExport {
    pub fn start<T, F>(config: ReverseExportConfig, tree_factory: F) -> Result<Self>
    where
        T: FileTree + Send + 'static,
        F: Fn() -> Result<T> + Send + Sync + 'static,
    {
        Self::start_server(config, move |stream, server| {
            let tree = tree_factory()?;
            serve_file_tree_connection(stream, server, tree)
        })
    }

    pub fn start_handler<H, F>(config: ReverseExportConfig, handler_factory: F) -> Result<Self>
    where
        H: ConnectionHandler,
        F: Fn() -> Result<H> + Send + Sync + 'static,
    {
        Self::start_server(config, move |stream, server| {
            let handler = Arc::new(handler_factory()?);
            serve_connection(stream, server, handler)
        })
    }

    pub fn start_authenticated<T, F>(
        config: ReverseExportConfig,
        session_auth: SessionAuthConfig,
        authentication_timeout: Duration,
        tree_factory: F,
    ) -> Result<Self>
    where
        T: FileTree + Send + 'static,
        F: Fn() -> Result<T> + Send + Sync + 'static,
    {
        Self::start_server(config, move |stream, mut server| {
            let session = authenticate_server(stream, &session_auth, authentication_timeout)?;
            server.session_uname = Some(session.peer.principal().as_bytes().to_vec());
            let tree = tree_factory()?;
            serve_file_tree_connection(session.stream, server, tree)
        })
    }

    pub fn start_authenticated_handler<H, F>(
        config: ReverseExportConfig,
        session_auth: SessionAuthConfig,
        authentication_timeout: Duration,
        handler_factory: F,
    ) -> Result<Self>
    where
        H: ConnectionHandler,
        F: Fn() -> Result<H> + Send + Sync + 'static,
    {
        Self::start_server(config, move |stream, mut server| {
            let session = authenticate_server(stream, &session_auth, authentication_timeout)?;
            server.session_uname = Some(session.peer.principal().as_bytes().to_vec());
            let handler = Arc::new(handler_factory()?);
            serve_connection(session.stream, server, handler)
        })
    }

    fn start_server<F>(config: ReverseExportConfig, serve: F) -> Result<Self>
    where
        F: Fn(SecureStream, ServerConfig) -> Result<()> + Send + Sync + 'static,
    {
        validate_config(&config)?;
        let state = ExportState {
            shutdown: Arc::new(AtomicBool::new(false)),
            connected_streams: Arc::new(AtomicUsize::new(0)),
            lifecycle: Arc::new(ExportLifecycle::default()),
            connection_failures: Arc::new(AtomicU64::new(0)),
            authentication_failures: Arc::new(AtomicU64::new(0)),
            completed_sessions: Arc::new(AtomicU64::new(0)),
            active_streams: Arc::new(Mutex::new(
                std::iter::repeat_with(|| None)
                    .take(config.connection_pool)
                    .collect(),
            )),
        };
        let serve = Arc::new(serve);
        let mut threads = Vec::with_capacity(config.connection_pool);
        for index in 0..config.connection_pool {
            let config = config.clone();
            let worker_state = state.clone();
            let serve = Arc::clone(&serve);
            threads.push(
                thread::Builder::new()
                    .name(format!("r9p-reverse-export-{index}"))
                    .spawn(move || export_loop(config, index, worker_state, serve))
                    .map_err(|error| Error::from(format!("spawn reverse export: {error}")))?,
            );
        }
        Ok(Self { state, threads })
    }

    pub fn connected_streams(&self) -> usize {
        self.state.connected_streams.load(Ordering::Acquire)
    }

    pub fn wait_for_connected(&self, timeout: Duration) -> bool {
        self.state.wait_for_connected(timeout)
    }

    pub fn status(&self) -> ReverseExportStatus {
        ReverseExportStatus {
            connected_streams: self.state.connected_streams.load(Ordering::Acquire),
            connection_failures: self.state.connection_failures.load(Ordering::Acquire),
            authentication_failures: self.state.authentication_failures.load(Ordering::Acquire),
            completed_sessions: self.state.completed_sessions.load(Ordering::Acquire),
        }
    }
}

impl Drop for ReverseExport {
    fn drop(&mut self) {
        self.state.begin_shutdown();
        if let Ok(streams) = self.state.active_streams.lock() {
            for stream in streams.iter().flatten() {
                let _ = stream.shutdown();
            }
        }
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn validate_config(config: &ReverseExportConfig) -> Result<()> {
    if config.broker_endpoint.port() == 0
        || config.broker_endpoint.is_unspecified_ip()
        || config.principal.is_empty()
        || config.principal.len() > 255
        || config.principal.bytes().any(|byte| byte.is_ascii_control())
        || config.connection_pool == 0
        || config.connection_pool > 256
        || config.connect_timeout.is_zero()
        || config.authentication_timeout.is_zero()
        || config.reconnect_min_delay.is_zero()
        || config.reconnect_max_delay < config.reconnect_min_delay
        || config.server.default_msize < 1024
        || config.server.max_msize < config.server.default_msize
        || config.server.max_fids == 0
        || config.server.max_async_requests == 0
    {
        return Err(Error::from(
            "reverse export requires a broker endpoint, bounded pool, principal, finite timeouts, and bounded 9P server configuration",
        ));
    }
    Ok(())
}

fn export_loop<F>(
    config: ReverseExportConfig,
    worker_index: usize,
    state: ExportState,
    serve: Arc<F>,
) where
    F: Fn(SecureStream, ServerConfig) -> Result<()> + Send + Sync + 'static,
{
    let mut failed_attempts = 0_u32;
    while !state.shutdown.load(Ordering::Acquire) {
        let stream = match connect_broker(&config.broker_endpoint, config.connect_timeout).and_then(
            |stream| {
                configure_transport_socket(&stream)?;
                Ok(stream)
            },
        ) {
            Ok(stream) => stream,
            Err(_) => {
                state.connection_failures.fetch_add(1, Ordering::AcqRel);
                state.wait_before_retry(retry_delay(&config, worker_index, failed_attempts));
                failed_attempts = failed_attempts.saturating_add(1);
                continue;
            }
        };
        let mut stream = match authenticate_client_to(
            stream,
            &config.auth,
            &config.expected_responder,
            &config.principal,
            config.authentication_timeout,
        ) {
            Ok(stream) => stream,
            Err(_) => {
                state.authentication_failures.fetch_add(1, Ordering::AcqRel);
                state.wait_before_retry(retry_delay(&config, worker_index, failed_attempts));
                failed_attempts = failed_attempts.saturating_add(1);
                continue;
            }
        };
        failed_attempts = 0;
        let shutdown_stream = match stream.try_clone() {
            Ok(stream) => stream,
            Err(_) => {
                state.connection_failures.fetch_add(1, Ordering::AcqRel);
                continue;
            }
        };
        if let Ok(mut streams) = state.active_streams.lock() {
            if state.shutdown.load(Ordering::Acquire) {
                let _ = stream.shutdown();
                break;
            }
            streams[worker_index] = Some(shutdown_stream);
        } else {
            let _ = stream.shutdown();
            return;
        }
        state.stream_connected();
        if receive_session_claim(&mut stream).is_ok() && !state.shutdown.load(Ordering::Acquire) {
            let _ = serve(stream, config.server.clone());
        }
        if let Ok(mut streams) = state.active_streams.lock() {
            streams[worker_index] = None;
        }
        state.stream_disconnected();
        state.completed_sessions.fetch_add(1, Ordering::AcqRel);
    }
}

fn connect_broker(endpoint: &TcpEndpoint, timeout: Duration) -> io::Result<TcpStream> {
    let addresses = (endpoint.host(), endpoint.port()).to_socket_addrs()?;
    let mut last_error = None;
    for address in addresses {
        match TcpStream::connect_timeout(&address, timeout) {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some((address, error)),
        }
    }
    match last_error {
        Some((address, error)) => Err(io::Error::new(
            error.kind(),
            format!("connect {endpoint} via {address}: {error}"),
        )),
        None => Err(io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            format!("{endpoint} resolved no addresses"),
        )),
    }
}

fn retry_delay(
    config: &ReverseExportConfig,
    worker_index: usize,
    failed_attempts: u32,
) -> Duration {
    let exponent = failed_attempts.min(12);
    let multiplier = 1_u32 << exponent;
    let base = config
        .reconnect_min_delay
        .saturating_mul(multiplier)
        .min(config.reconnect_max_delay);
    let divisor = u32::try_from(config.connection_pool).unwrap_or(u32::MAX);
    let phase = u32::try_from(worker_index)
        .ok()
        .and_then(|index| config.reconnect_min_delay.checked_mul(index))
        .map(|spread| spread / divisor)
        .unwrap_or_default();
    base.saturating_add(phase).min(config.reconnect_max_delay)
}

impl ExportState {
    fn wait_for_connected(&self, timeout: Duration) -> bool {
        let deadline = Instant::now().checked_add(timeout);
        let mut gate = self.lock_lifecycle();
        loop {
            if self.connected_streams.load(Ordering::Acquire) > 0 {
                return true;
            }
            if self.shutdown.load(Ordering::Acquire) {
                return false;
            }
            let Some(deadline) = deadline else {
                return false;
            };
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            let (next, wait) = self
                .lifecycle
                .changed
                .wait_timeout(gate, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            gate = next;
            if wait.timed_out() && self.connected_streams.load(Ordering::Acquire) == 0 {
                return false;
            }
        }
    }

    fn wait_before_retry(&self, duration: Duration) {
        if duration.is_zero() || self.shutdown.load(Ordering::Acquire) {
            return;
        }
        let gate = self.lock_lifecycle();
        let _wait = self
            .lifecycle
            .changed
            .wait_timeout_while(gate, duration, |_| !self.shutdown.load(Ordering::Acquire))
            .unwrap_or_else(|poisoned| poisoned.into_inner());
    }

    fn stream_connected(&self) {
        let _gate = self.lock_lifecycle();
        self.connected_streams.fetch_add(1, Ordering::AcqRel);
        self.lifecycle.changed.notify_all();
    }

    fn stream_disconnected(&self) {
        let _gate = self.lock_lifecycle();
        self.connected_streams.fetch_sub(1, Ordering::AcqRel);
        self.lifecycle.changed.notify_all();
    }

    fn begin_shutdown(&self) {
        let _gate = self.lock_lifecycle();
        self.shutdown.store(true, Ordering::Release);
        self.lifecycle.changed.notify_all();
    }

    fn lock_lifecycle(&self) -> MutexGuard<'_, ()> {
        self.lifecycle
            .gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
