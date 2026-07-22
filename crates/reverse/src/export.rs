use std::{
    net::{SocketAddr, TcpStream},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use fs::{LocalTree, LocalTreeConfig};
use r9p::{
    codec::Variant,
    error::{Error, Result},
    server::{serve_file_tree_connection, ServerConfig},
};
use r9p_auth::{authenticate_client, ClientConfig};

#[derive(Clone)]
pub struct FilesystemExportConfig {
    pub broker_endpoint: SocketAddr,
    pub auth: ClientConfig,
    pub principal: String,
    pub root: PathBuf,
    pub writable: bool,
    pub connection_pool: usize,
    pub connect_timeout: Duration,
    pub authentication_timeout: Duration,
    pub reconnect_delay: Duration,
    pub msize: u32,
    pub max_fids: usize,
}

pub struct FilesystemExport {
    shutdown: Arc<AtomicBool>,
    connected_streams: Arc<AtomicUsize>,
    endpoint: SocketAddr,
    active_streams: Arc<Mutex<Vec<Option<r9p_auth::SecureStream>>>>,
    threads: Vec<JoinHandle<()>>,
}

impl FilesystemExport {
    pub fn start(config: FilesystemExportConfig) -> Result<Self> {
        validate_config(&config)?;
        LocalTree::open_with_config(
            &config.root,
            LocalTreeConfig {
                writable: config.writable,
            },
        )?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let connected_streams = Arc::new(AtomicUsize::new(0));
        let active_streams = Arc::new(Mutex::new(
            std::iter::repeat_with(|| None)
                .take(config.connection_pool)
                .collect(),
        ));
        let mut threads = Vec::with_capacity(config.connection_pool);
        for index in 0..config.connection_pool {
            let config = config.clone();
            let worker_shutdown = Arc::clone(&shutdown);
            let worker_connected = Arc::clone(&connected_streams);
            let worker_streams = Arc::clone(&active_streams);
            threads.push(
                thread::Builder::new()
                    .name(format!("r9p-reverse-export-{index}"))
                    .spawn(move || {
                        export_loop(
                            config,
                            index,
                            worker_shutdown,
                            worker_connected,
                            worker_streams,
                        )
                    })
                    .map_err(|error| Error::from(format!("spawn reverse export: {error}")))?,
            );
        }
        Ok(Self {
            shutdown,
            connected_streams,
            endpoint: config.broker_endpoint,
            active_streams,
            threads,
        })
    }

    pub fn connected_streams(&self) -> usize {
        self.connected_streams.load(Ordering::Acquire)
    }
}

impl Drop for FilesystemExport {
    fn drop(&mut self) {
        if let Ok(streams) = self.active_streams.lock() {
            self.shutdown.store(true, Ordering::Release);
            for stream in streams.iter() {
                if let Some(stream) = stream {
                    let _ = stream.shutdown();
                }
            }
        } else {
            self.shutdown.store(true, Ordering::Release);
        }
        let _ = TcpStream::connect(self.endpoint);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn validate_config(config: &FilesystemExportConfig) -> Result<()> {
    if config.principal.is_empty()
        || config.principal.len() > 255
        || config.principal.bytes().any(|byte| byte.is_ascii_control())
        || config.connection_pool == 0
        || config.connection_pool > 256
        || config.connect_timeout.is_zero()
        || config.authentication_timeout.is_zero()
        || config.reconnect_delay.is_zero()
        || config.msize < 1024
        || config.max_fids == 0
    {
        return Err(Error::from(
            "reverse export requires a broker endpoint, bounded pool, principal, finite timeouts, msize, and fid limit",
        ));
    }
    Ok(())
}

fn export_loop(
    config: FilesystemExportConfig,
    worker_index: usize,
    shutdown: Arc<AtomicBool>,
    connected: Arc<AtomicUsize>,
    active_streams: Arc<Mutex<Vec<Option<r9p_auth::SecureStream>>>>,
) {
    while !shutdown.load(Ordering::Acquire) {
        let stream =
            match TcpStream::connect_timeout(&config.broker_endpoint, config.connect_timeout) {
                Ok(stream) => stream,
                Err(_) => {
                    sleep_until_retry(&shutdown, config.reconnect_delay);
                    continue;
                }
            };
        let stream = match authenticate_client(
            stream,
            &config.auth,
            &config.principal,
            config.authentication_timeout,
        ) {
            Ok(stream) => stream,
            Err(_) => {
                sleep_until_retry(&shutdown, config.reconnect_delay);
                continue;
            }
        };
        let shutdown_stream = match stream.try_clone() {
            Ok(stream) => stream,
            Err(_) => {
                sleep_until_retry(&shutdown, config.reconnect_delay);
                continue;
            }
        };
        if let Ok(mut streams) = active_streams.lock() {
            if shutdown.load(Ordering::Acquire) {
                let _ = stream.shutdown();
                break;
            }
            streams[worker_index] = Some(shutdown_stream);
        } else {
            let _ = stream.shutdown();
            return;
        }
        connected.fetch_add(1, Ordering::AcqRel);
        let tree = LocalTree::open_with_config(
            &config.root,
            LocalTreeConfig {
                writable: config.writable,
            },
        );
        if let Ok(tree) = tree {
            let _ = serve_file_tree_connection(
                stream,
                ServerConfig {
                    default_msize: config.msize,
                    max_msize: config.msize,
                    max_fids: config.max_fids,
                    variant: Variant::R9pSymlink,
                    ..ServerConfig::default()
                },
                tree,
            );
        }
        if let Ok(mut streams) = active_streams.lock() {
            streams[worker_index] = None;
        }
        connected.fetch_sub(1, Ordering::AcqRel);
        sleep_until_retry(&shutdown, config.reconnect_delay);
    }
}

fn sleep_until_retry(shutdown: &AtomicBool, duration: Duration) {
    let quantum = Duration::from_millis(25);
    let mut remaining = duration;
    while !shutdown.load(Ordering::Acquire) && !remaining.is_zero() {
        let delay = remaining.min(quantum);
        thread::sleep(delay);
        remaining = remaining.saturating_sub(delay);
    }
}
