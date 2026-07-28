use std::{
    io,
    net::{Shutdown, SocketAddr, TcpStream},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use r9p::error::{Error, Result};
use r9p_auth::{authenticate_client, ClientConfig};

use crate::{
    broker::{bridge, LocalStream, ProxyEndpoint, ProxyListener},
    configure_transport_socket,
};

#[derive(Clone)]
pub struct SessionProxyConfig {
    pub bind: ProxyEndpoint,
    pub upstream: SocketAddr,
    pub auth: ClientConfig,
    pub principal: String,
    pub max_sessions: usize,
    pub connect_timeout: Duration,
    pub authentication_timeout: Duration,
}

pub struct SessionProxy {
    endpoint: ProxyEndpoint,
    counters: Arc<SessionProxyCounters>,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionProxyStatus {
    pub active_sessions: usize,
    pub accepted_sessions: u64,
    pub rejected_sessions: u64,
    pub connection_failures: u64,
    pub authentication_failures: u64,
    pub completed_sessions: u64,
}

#[derive(Default)]
struct SessionProxyCounters {
    active_sessions: AtomicUsize,
    accepted_sessions: AtomicU64,
    rejected_sessions: AtomicU64,
    connection_failures: AtomicU64,
    authentication_failures: AtomicU64,
    completed_sessions: AtomicU64,
}

impl SessionProxy {
    pub fn start(config: SessionProxyConfig) -> Result<Self> {
        validate_config(&config)?;
        let listener = ProxyListener::bind(&config.bind)?;
        let endpoint = listener.local_endpoint()?;
        let counters = Arc::new(SessionProxyCounters::default());
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread = spawn_acceptor(
            listener,
            config,
            Arc::clone(&counters),
            Arc::clone(&shutdown),
        )?;
        Ok(Self {
            endpoint,
            counters,
            shutdown,
            thread: Some(thread),
        })
    }

    pub const fn endpoint(&self) -> &ProxyEndpoint {
        &self.endpoint
    }

    pub fn status(&self) -> SessionProxyStatus {
        SessionProxyStatus {
            active_sessions: self.counters.active_sessions.load(Ordering::Acquire),
            accepted_sessions: self.counters.accepted_sessions.load(Ordering::Acquire),
            rejected_sessions: self.counters.rejected_sessions.load(Ordering::Acquire),
            connection_failures: self.counters.connection_failures.load(Ordering::Acquire),
            authentication_failures: self
                .counters
                .authentication_failures
                .load(Ordering::Acquire),
            completed_sessions: self.counters.completed_sessions.load(Ordering::Acquire),
        }
    }
}

impl Drop for SessionProxy {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = ProxyListener::wake(&self.endpoint);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if let ProxyEndpoint::Unix(path) = &self.endpoint {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn validate_config(config: &SessionProxyConfig) -> Result<()> {
    let local_bind = match &config.bind {
        ProxyEndpoint::Tcp(address) => address.ip().is_loopback(),
        ProxyEndpoint::Unix(path) => {
            path.is_absolute() && path.file_name().is_some() && path.as_os_str().len() <= 4096
        }
    };
    if !local_bind
        || config.principal.is_empty()
        || config.principal.len() > 255
        || config.principal.bytes().any(|byte| byte.is_ascii_control())
        || config.max_sessions == 0
        || config.max_sessions > 256
        || config.connect_timeout.is_zero()
        || config.authentication_timeout.is_zero()
    {
        return Err(Error::from(
            "session proxy requires a local bind, bounded sessions, a principal, and finite timeouts",
        ));
    }
    Ok(())
}

fn spawn_acceptor(
    listener: ProxyListener,
    config: SessionProxyConfig,
    counters: Arc<SessionProxyCounters>,
    shutdown: Arc<AtomicBool>,
) -> Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("r9p-session-proxy".to_string())
        .spawn(move || {
            while !shutdown.load(Ordering::Acquire) {
                let local = match listener.accept() {
                    Ok(stream) => stream,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                };
                if shutdown.load(Ordering::Acquire) {
                    let _ = local.shutdown();
                    break;
                }
                if local.configure().is_err() {
                    counters.rejected_sessions.fetch_add(1, Ordering::AcqRel);
                    let _ = local.shutdown();
                    continue;
                }
                let Some(slot) =
                    SessionSlot::try_acquire(Arc::clone(&counters), config.max_sessions)
                else {
                    counters.rejected_sessions.fetch_add(1, Ordering::AcqRel);
                    let _ = local.shutdown();
                    continue;
                };
                counters.accepted_sessions.fetch_add(1, Ordering::AcqRel);
                let worker_config = config.clone();
                let worker_counters = Arc::clone(&counters);
                if thread::Builder::new()
                    .name("r9p-session-proxy-bridge".to_string())
                    .spawn(move || {
                        let _slot = slot;
                        proxy_session(local, &worker_config, &worker_counters);
                    })
                    .is_err()
                {
                    counters.rejected_sessions.fetch_add(1, Ordering::AcqRel);
                }
            }
        })
        .map_err(|error| Error::from(format!("spawn session proxy acceptor: {error}")))
}

fn proxy_session(local: LocalStream, config: &SessionProxyConfig, counters: &SessionProxyCounters) {
    let upstream = match TcpStream::connect_timeout(&config.upstream, config.connect_timeout) {
        Ok(stream) => stream,
        Err(_) => {
            counters.connection_failures.fetch_add(1, Ordering::AcqRel);
            let _ = local.shutdown();
            return;
        }
    };
    if configure_transport_socket(&upstream).is_err() {
        counters.connection_failures.fetch_add(1, Ordering::AcqRel);
        let _ = upstream.shutdown(Shutdown::Both);
        let _ = local.shutdown();
        return;
    }
    let stream = match authenticate_client(
        upstream,
        &config.auth,
        &config.principal,
        config.authentication_timeout,
    ) {
        Ok(session) => session,
        Err(_) => {
            counters
                .authentication_failures
                .fetch_add(1, Ordering::AcqRel);
            let _ = local.shutdown();
            return;
        }
    };
    let _ = bridge(local, stream);
    counters.completed_sessions.fetch_add(1, Ordering::AcqRel);
}

struct SessionSlot {
    counters: Arc<SessionProxyCounters>,
}

impl SessionSlot {
    fn try_acquire(counters: Arc<SessionProxyCounters>, limit: usize) -> Option<Self> {
        counters
            .active_sessions
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                (value < limit).then_some(value + 1)
            })
            .ok()?;
        Some(Self { counters })
    }
}

impl Drop for SessionSlot {
    fn drop(&mut self) {
        self.counters.active_sessions.fetch_sub(1, Ordering::AcqRel);
    }
}
