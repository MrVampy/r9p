use std::{
    fmt, io,
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};

use r9p::error::{Error, Result};
use r9p_auth::{authenticate_server, SecureStream, ServerConfig};

use crate::configure_transport_socket;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProxyEndpoint {
    Tcp(SocketAddr),
    Unix(PathBuf),
}

impl ProxyEndpoint {
    pub const fn tcp(address: SocketAddr) -> Self {
        Self::Tcp(address)
    }

    pub fn unix(path: impl Into<PathBuf>) -> Self {
        Self::Unix(path.into())
    }

    pub const fn as_tcp(&self) -> Option<SocketAddr> {
        match self {
            Self::Tcp(address) => Some(*address),
            Self::Unix(_) => None,
        }
    }

    pub fn as_unix(&self) -> Option<&Path> {
        match self {
            Self::Tcp(_) => None,
            Self::Unix(path) => Some(path),
        }
    }
}

impl fmt::Display for ProxyEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tcp(address) => address.fmt(formatter),
            Self::Unix(path) => write!(formatter, "unix!{}", path.display()),
        }
    }
}

#[derive(Clone)]
pub struct BrokerConfig {
    pub reverse_bind: SocketAddr,
    pub proxy_bind: ProxyEndpoint,
    pub auth: ServerConfig,
    pub peer_principal: String,
    pub max_waiting_streams: usize,
    pub authentication_timeout: Duration,
    pub proxy_wait_timeout: Duration,
}

pub struct ReverseBroker {
    reverse_endpoint: SocketAddr,
    proxy_endpoint: ProxyEndpoint,
    counters: Arc<BrokerCounters>,
    shutdown: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BrokerStatus {
    pub waiting_streams: usize,
    pub active_handshakes: usize,
    pub active_bridges: usize,
    pub rejected_reverse_connections: u64,
    pub authentication_failures: u64,
    pub unavailable_proxy_connections: u64,
    pub completed_bridges: u64,
}

#[derive(Default)]
struct BrokerCounters {
    waiting_streams: AtomicUsize,
    active_handshakes: AtomicUsize,
    active_bridges: AtomicUsize,
    rejected_reverse_connections: AtomicU64,
    authentication_failures: AtomicU64,
    unavailable_proxy_connections: AtomicU64,
    completed_bridges: AtomicU64,
}

impl ReverseBroker {
    pub fn start(config: BrokerConfig) -> Result<Self> {
        validate_config(&config)?;
        let reverse_listener = TcpListener::bind(config.reverse_bind)
            .map_err(|error| io_error("bind reverse listener", error))?;
        let proxy_listener = ProxyListener::bind(&config.proxy_bind)?;
        reverse_listener
            .set_nonblocking(true)
            .map_err(|error| io_error("configure reverse listener", error))?;
        proxy_listener
            .set_nonblocking(true)
            .map_err(|error| io_error("configure proxy listener", error))?;
        let reverse_endpoint = reverse_listener
            .local_addr()
            .map_err(|error| io_error("inspect reverse listener", error))?;
        let proxy_endpoint = proxy_listener.local_endpoint()?;
        let (sender, receiver) = mpsc::sync_channel(config.max_waiting_streams);
        let counters = Arc::new(BrokerCounters::default());
        let shutdown = Arc::new(AtomicBool::new(false));

        let reverse_thread = spawn_reverse_acceptor(
            reverse_listener,
            config.clone(),
            sender,
            Arc::clone(&counters),
            Arc::clone(&shutdown),
        )?;
        let proxy_thread = spawn_proxy_acceptor(
            proxy_listener,
            config.proxy_wait_timeout,
            config.max_waiting_streams,
            receiver,
            Arc::clone(&counters),
            Arc::clone(&shutdown),
        )?;

        Ok(Self {
            reverse_endpoint,
            proxy_endpoint,
            counters,
            shutdown,
            threads: vec![reverse_thread, proxy_thread],
        })
    }

    pub const fn reverse_endpoint(&self) -> SocketAddr {
        self.reverse_endpoint
    }

    pub const fn proxy_endpoint(&self) -> &ProxyEndpoint {
        &self.proxy_endpoint
    }

    pub fn waiting_streams(&self) -> usize {
        self.counters.waiting_streams.load(Ordering::Acquire)
    }

    pub fn is_ready(&self) -> bool {
        self.waiting_streams() > 0
    }

    pub fn status(&self) -> BrokerStatus {
        BrokerStatus {
            waiting_streams: self.counters.waiting_streams.load(Ordering::Acquire),
            active_handshakes: self.counters.active_handshakes.load(Ordering::Acquire),
            active_bridges: self.counters.active_bridges.load(Ordering::Acquire),
            rejected_reverse_connections: self
                .counters
                .rejected_reverse_connections
                .load(Ordering::Acquire),
            authentication_failures: self
                .counters
                .authentication_failures
                .load(Ordering::Acquire),
            unavailable_proxy_connections: self
                .counters
                .unavailable_proxy_connections
                .load(Ordering::Acquire),
            completed_bridges: self.counters.completed_bridges.load(Ordering::Acquire),
        }
    }
}

impl Drop for ReverseBroker {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.reverse_endpoint);
        let _ = ProxyListener::wake(&self.proxy_endpoint);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
        if let ProxyEndpoint::Unix(path) = &self.proxy_endpoint {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn validate_config(config: &BrokerConfig) -> Result<()> {
    let proxy_is_local = match &config.proxy_bind {
        ProxyEndpoint::Tcp(address) => address.ip().is_loopback(),
        ProxyEndpoint::Unix(path) => {
            path.is_absolute() && path.file_name().is_some() && path.as_os_str().len() <= 4096
        }
    };
    if !proxy_is_local
        || config.peer_principal.is_empty()
        || config.peer_principal.len() > 255
        || config
            .peer_principal
            .bytes()
            .any(|byte| byte.is_ascii_control())
        || config.max_waiting_streams == 0
        || config.max_waiting_streams > 256
        || config.authentication_timeout.is_zero()
        || config.proxy_wait_timeout.is_zero()
    {
        return Err(Error::from(
            "reverse broker requires a network reverse bind, local proxy endpoint, bounded queue, principal, and finite timeouts",
        ));
    }
    Ok(())
}

fn spawn_reverse_acceptor(
    listener: TcpListener,
    config: BrokerConfig,
    sender: SyncSender<SecureStream>,
    counters: Arc<BrokerCounters>,
    shutdown: Arc<AtomicBool>,
) -> Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("r9p-reverse-auth".to_string())
        .spawn(move || {
            while !shutdown.load(Ordering::Acquire) {
                let stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(25));
                        continue;
                    }
                    Err(_) => break,
                };
                if shutdown.load(Ordering::Acquire) {
                    break;
                }
                if configure_transport_socket(&stream).is_err() {
                    counters
                        .rejected_reverse_connections
                        .fetch_add(1, Ordering::AcqRel);
                    let _ = stream.shutdown(Shutdown::Both);
                    continue;
                }
                let Some(handshake_slot) = CounterSlot::try_acquire(
                    Arc::clone(&counters),
                    CounterKind::Handshake,
                    config.max_waiting_streams,
                ) else {
                    counters
                        .rejected_reverse_connections
                        .fetch_add(1, Ordering::AcqRel);
                    let _ = stream.shutdown(Shutdown::Both);
                    continue;
                };
                let auth = config.auth.clone();
                let principal = config.peer_principal.clone();
                let sender = sender.clone();
                let handshake_counters = Arc::clone(&counters);
                let timeout = config.authentication_timeout;
                if thread::Builder::new()
                    .name("r9p-reverse-handshake".to_string())
                    .spawn(move || {
                        let _handshake_slot = handshake_slot;
                        let Ok(session) = authenticate_server(stream, &auth, timeout) else {
                            handshake_counters
                                .authentication_failures
                                .fetch_add(1, Ordering::AcqRel);
                            return;
                        };
                        if session.peer.principal() != principal {
                            handshake_counters
                                .authentication_failures
                                .fetch_add(1, Ordering::AcqRel);
                            let _ = session.stream.shutdown();
                            return;
                        }
                        handshake_counters
                            .waiting_streams
                            .fetch_add(1, Ordering::AcqRel);
                        match sender.try_send(session.stream) {
                            Ok(()) => {}
                            Err(TrySendError::Full(stream))
                            | Err(TrySendError::Disconnected(stream)) => {
                                handshake_counters
                                    .waiting_streams
                                    .fetch_sub(1, Ordering::AcqRel);
                                let _ = stream.shutdown();
                            }
                        }
                    })
                    .is_err()
                {
                    counters
                        .rejected_reverse_connections
                        .fetch_add(1, Ordering::AcqRel);
                }
            }
        })
        .map_err(|error| Error::from(format!("spawn reverse acceptor: {error}")))
}

fn spawn_proxy_acceptor(
    listener: ProxyListener,
    wait_timeout: Duration,
    max_active_bridges: usize,
    receiver: Receiver<SecureStream>,
    counters: Arc<BrokerCounters>,
    shutdown: Arc<AtomicBool>,
) -> Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("r9p-reverse-proxy".to_string())
        .spawn(move || {
            while !shutdown.load(Ordering::Acquire) {
                let local = match listener.accept() {
                    Ok(stream) => stream,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(25));
                        continue;
                    }
                    Err(_) => break,
                };
                if shutdown.load(Ordering::Acquire) {
                    break;
                }
                if local.configure().is_err() {
                    counters
                        .unavailable_proxy_connections
                        .fetch_add(1, Ordering::AcqRel);
                    let _ = local.shutdown();
                    continue;
                }
                let Some(bridge_slot) = CounterSlot::try_acquire(
                    Arc::clone(&counters),
                    CounterKind::Bridge,
                    max_active_bridges,
                ) else {
                    counters
                        .unavailable_proxy_connections
                        .fetch_add(1, Ordering::AcqRel);
                    let _ = local.shutdown();
                    continue;
                };
                let Some(remote) =
                    receive_live_stream(&receiver, wait_timeout, &counters, &shutdown)
                else {
                    counters
                        .unavailable_proxy_connections
                        .fetch_add(1, Ordering::AcqRel);
                    let _ = local.shutdown();
                    continue;
                };
                let bridge_counters = Arc::clone(&counters);
                if thread::Builder::new()
                    .name("r9p-reverse-bridge".to_string())
                    .spawn(move || {
                        let _bridge_slot = bridge_slot;
                        let _ = bridge(local, remote);
                        bridge_counters
                            .completed_bridges
                            .fetch_add(1, Ordering::AcqRel);
                    })
                    .is_err()
                {
                    counters
                        .unavailable_proxy_connections
                        .fetch_add(1, Ordering::AcqRel);
                }
            }
        })
        .map_err(|error| Error::from(format!("spawn proxy acceptor: {error}")))
}

fn receive_live_stream(
    receiver: &Receiver<SecureStream>,
    wait_timeout: Duration,
    counters: &BrokerCounters,
    shutdown: &AtomicBool,
) -> Option<SecureStream> {
    let deadline = std::time::Instant::now() + wait_timeout;
    while !shutdown.load(Ordering::Acquire) {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        let stream = match receiver.recv_timeout(remaining) {
            Ok(stream) => stream,
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return None,
        };
        counters.waiting_streams.fetch_sub(1, Ordering::AcqRel);
        match stream.peer_closed(Duration::from_millis(2)) {
            Ok(true) | Err(_) => {
                let _ = stream.shutdown();
            }
            Ok(false) => return Some(stream),
        }
    }
    None
}

enum CounterKind {
    Handshake,
    Bridge,
}

struct CounterSlot {
    counters: Arc<BrokerCounters>,
    kind: CounterKind,
}

impl CounterSlot {
    fn try_acquire(counters: Arc<BrokerCounters>, kind: CounterKind, limit: usize) -> Option<Self> {
        let counter = match kind {
            CounterKind::Handshake => &counters.active_handshakes,
            CounterKind::Bridge => &counters.active_bridges,
        };
        counter
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                (value < limit).then_some(value + 1)
            })
            .ok()?;
        Some(Self { counters, kind })
    }
}

impl Drop for CounterSlot {
    fn drop(&mut self) {
        match self.kind {
            CounterKind::Handshake => {
                self.counters
                    .active_handshakes
                    .fetch_sub(1, Ordering::AcqRel);
            }
            CounterKind::Bridge => {
                self.counters.active_bridges.fetch_sub(1, Ordering::AcqRel);
            }
        }
    }
}

fn bridge(local: LocalStream, remote: SecureStream) -> io::Result<()> {
    let mut local_reader = local.try_clone()?;
    let mut local_writer = local;
    let mut remote_reader = remote.try_clone()?;
    let mut remote_writer = remote;
    let local_shutdown = local_reader.try_clone()?;
    let remote_shutdown = remote_reader.try_clone()?;

    let upstream = thread::spawn(move || {
        let result = io::copy(&mut local_reader, &mut remote_writer);
        let _ = remote_writer.shutdown();
        result
    });
    let downstream = io::copy(&mut remote_reader, &mut local_writer);
    let _ = local_shutdown.shutdown();
    let _ = remote_shutdown.shutdown();
    let upstream = upstream
        .join()
        .map_err(|_| io::Error::other("reverse bridge worker panicked"))?;
    upstream?;
    downstream.map(|_| ())
}

fn io_error(context: &str, error: io::Error) -> Error {
    Error::from(format!("{context}: {error}"))
}

enum ProxyListener {
    Tcp(TcpListener),
    #[cfg(unix)]
    Unix {
        listener: UnixListener,
        path: PathBuf,
    },
}

impl ProxyListener {
    fn bind(endpoint: &ProxyEndpoint) -> Result<Self> {
        match endpoint {
            ProxyEndpoint::Tcp(address) => TcpListener::bind(address)
                .map(Self::Tcp)
                .map_err(|error| io_error("bind TCP proxy listener", error)),
            ProxyEndpoint::Unix(path) => Self::bind_unix(path),
        }
    }

    #[cfg(unix)]
    fn bind_unix(path: &Path) -> Result<Self> {
        UnixListener::bind(path)
            .map(|listener| Self::Unix {
                listener,
                path: path.to_path_buf(),
            })
            .map_err(|error| io_error("bind Unix proxy listener", error))
    }

    #[cfg(not(unix))]
    fn bind_unix(_path: &Path) -> Result<Self> {
        Err(Error::from("Unix proxy endpoints are not supported"))
    }

    fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        match self {
            Self::Tcp(listener) => listener.set_nonblocking(nonblocking),
            #[cfg(unix)]
            Self::Unix { listener, .. } => listener.set_nonblocking(nonblocking),
        }
    }

    fn local_endpoint(&self) -> Result<ProxyEndpoint> {
        match self {
            Self::Tcp(listener) => listener
                .local_addr()
                .map(ProxyEndpoint::Tcp)
                .map_err(|error| io_error("inspect TCP proxy listener", error)),
            #[cfg(unix)]
            Self::Unix { path, .. } => Ok(ProxyEndpoint::Unix(path.clone())),
        }
    }

    fn accept(&self) -> io::Result<LocalStream> {
        match self {
            Self::Tcp(listener) => listener
                .accept()
                .map(|(stream, _)| LocalStream::Tcp(stream)),
            #[cfg(unix)]
            Self::Unix { listener, .. } => listener
                .accept()
                .map(|(stream, _)| LocalStream::Unix(stream)),
        }
    }

    fn wake(endpoint: &ProxyEndpoint) -> io::Result<()> {
        match endpoint {
            ProxyEndpoint::Tcp(address) => TcpStream::connect(address).map(|_| ()),
            #[cfg(unix)]
            ProxyEndpoint::Unix(path) => UnixStream::connect(path).map(|_| ()),
            #[cfg(not(unix))]
            ProxyEndpoint::Unix(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Unix proxy endpoints are not supported",
            )),
        }
    }
}

enum LocalStream {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(UnixStream),
}

impl LocalStream {
    fn configure(&self) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => configure_transport_socket(stream),
            #[cfg(unix)]
            Self::Unix(_) => Ok(()),
        }
    }

    fn try_clone(&self) -> io::Result<Self> {
        match self {
            Self::Tcp(stream) => stream.try_clone().map(Self::Tcp),
            #[cfg(unix)]
            Self::Unix(stream) => stream.try_clone().map(Self::Unix),
        }
    }

    fn shutdown(&self) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.shutdown(Shutdown::Both),
            #[cfg(unix)]
            Self::Unix(stream) => stream.shutdown(Shutdown::Both),
        }
    }
}

impl io::Read for LocalStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => io::Read::read(stream, buffer),
            #[cfg(unix)]
            Self::Unix(stream) => io::Read::read(stream, buffer),
        }
    }
}

impl io::Write for LocalStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => io::Write::write(stream, buffer),
            #[cfg(unix)]
            Self::Unix(stream) => io::Write::write(stream, buffer),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => io::Write::flush(stream),
            #[cfg(unix)]
            Self::Unix(stream) => io::Write::flush(stream),
        }
    }
}
