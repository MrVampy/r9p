use std::{
    io,
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use r9p::error::{Error, Result};
use r9p_auth::{authenticate_server, SecureStream, ServerConfig};

#[derive(Clone)]
pub struct BrokerConfig {
    pub reverse_bind: SocketAddr,
    pub proxy_bind: SocketAddr,
    pub auth: ServerConfig,
    pub peer_principal: String,
    pub max_waiting_streams: usize,
    pub authentication_timeout: Duration,
    pub proxy_wait_timeout: Duration,
}

pub struct ReverseBroker {
    reverse_endpoint: SocketAddr,
    proxy_endpoint: SocketAddr,
    waiting_streams: Arc<AtomicUsize>,
    shutdown: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl ReverseBroker {
    pub fn start(config: BrokerConfig) -> Result<Self> {
        validate_config(&config)?;
        let reverse_listener = TcpListener::bind(config.reverse_bind)
            .map_err(|error| io_error("bind reverse listener", error))?;
        let proxy_listener = TcpListener::bind(config.proxy_bind)
            .map_err(|error| io_error("bind proxy listener", error))?;
        reverse_listener
            .set_nonblocking(true)
            .map_err(|error| io_error("configure reverse listener", error))?;
        proxy_listener
            .set_nonblocking(true)
            .map_err(|error| io_error("configure proxy listener", error))?;
        let reverse_endpoint = reverse_listener
            .local_addr()
            .map_err(|error| io_error("inspect reverse listener", error))?;
        let proxy_endpoint = proxy_listener
            .local_addr()
            .map_err(|error| io_error("inspect proxy listener", error))?;
        let (sender, receiver) = mpsc::sync_channel(config.max_waiting_streams);
        let receiver = Arc::new(Mutex::new(receiver));
        let waiting_streams = Arc::new(AtomicUsize::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));

        let reverse_thread = spawn_reverse_acceptor(
            reverse_listener,
            config.clone(),
            sender,
            Arc::clone(&waiting_streams),
            Arc::clone(&shutdown),
        )?;
        let proxy_thread = spawn_proxy_acceptor(
            proxy_listener,
            config.proxy_wait_timeout,
            receiver,
            Arc::clone(&waiting_streams),
            Arc::clone(&shutdown),
        )?;

        Ok(Self {
            reverse_endpoint,
            proxy_endpoint,
            waiting_streams,
            shutdown,
            threads: vec![reverse_thread, proxy_thread],
        })
    }

    pub const fn reverse_endpoint(&self) -> SocketAddr {
        self.reverse_endpoint
    }

    pub const fn proxy_endpoint(&self) -> SocketAddr {
        self.proxy_endpoint
    }

    pub fn waiting_streams(&self) -> usize {
        self.waiting_streams.load(Ordering::Acquire)
    }

    pub fn is_ready(&self) -> bool {
        self.waiting_streams() > 0
    }
}

impl Drop for ReverseBroker {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.reverse_endpoint);
        let _ = TcpStream::connect(self.proxy_endpoint);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn validate_config(config: &BrokerConfig) -> Result<()> {
    if !config.proxy_bind.ip().is_loopback()
        || config.peer_principal.is_empty()
        || config.peer_principal.len() > 255
        || config
            .peer_principal
            .bytes()
            .any(|byte| byte.is_ascii_control())
        || config.max_waiting_streams == 0
        || config.authentication_timeout.is_zero()
        || config.proxy_wait_timeout.is_zero()
    {
        return Err(Error::from(
            "reverse broker requires a network reverse bind, loopback proxy bind, bounded queue, principal, and finite timeouts",
        ));
    }
    Ok(())
}

fn spawn_reverse_acceptor(
    listener: TcpListener,
    config: BrokerConfig,
    sender: SyncSender<SecureStream>,
    waiting: Arc<AtomicUsize>,
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
                let auth = config.auth.clone();
                let principal = config.peer_principal.clone();
                let sender = sender.clone();
                let waiting = Arc::clone(&waiting);
                let timeout = config.authentication_timeout;
                let _ = thread::Builder::new()
                    .name("r9p-reverse-handshake".to_string())
                    .spawn(move || {
                        let Ok(session) = authenticate_server(stream, &auth, timeout) else {
                            return;
                        };
                        if session.peer.principal() != principal {
                            let _ = session.stream.shutdown();
                            return;
                        }
                        waiting.fetch_add(1, Ordering::AcqRel);
                        match sender.try_send(session.stream) {
                            Ok(()) => {}
                            Err(TrySendError::Full(stream))
                            | Err(TrySendError::Disconnected(stream)) => {
                                waiting.fetch_sub(1, Ordering::AcqRel);
                                let _ = stream.shutdown();
                            }
                        }
                    });
            }
        })
        .map_err(|error| Error::from(format!("spawn reverse acceptor: {error}")))
}

fn spawn_proxy_acceptor(
    listener: TcpListener,
    wait_timeout: Duration,
    receiver: Arc<Mutex<Receiver<SecureStream>>>,
    waiting: Arc<AtomicUsize>,
    shutdown: Arc<AtomicBool>,
) -> Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("r9p-reverse-proxy".to_string())
        .spawn(move || {
            while !shutdown.load(Ordering::Acquire) {
                let local = match listener.accept() {
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
                let receiver = Arc::clone(&receiver);
                let waiting = Arc::clone(&waiting);
                let _ = thread::Builder::new()
                    .name("r9p-reverse-bridge".to_string())
                    .spawn(move || {
                        let remote = receiver
                            .lock()
                            .ok()
                            .and_then(|receiver| receiver.recv_timeout(wait_timeout).ok());
                        let Some(remote) = remote else {
                            let _ = local.shutdown(Shutdown::Both);
                            return;
                        };
                        waiting.fetch_sub(1, Ordering::AcqRel);
                        let _ = bridge(local, remote);
                    });
            }
        })
        .map_err(|error| Error::from(format!("spawn proxy acceptor: {error}")))
}

fn bridge(local: TcpStream, remote: SecureStream) -> io::Result<()> {
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
    let _ = local_shutdown.shutdown(Shutdown::Both);
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
