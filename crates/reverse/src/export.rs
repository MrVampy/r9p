use std::{
    net::{SocketAddr, TcpStream},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
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
    pub reconnect_min_delay: Duration,
    pub reconnect_max_delay: Duration,
    pub msize: u32,
    pub max_fids: usize,
}

pub struct FilesystemExport {
    state: ExportState,
    threads: Vec<JoinHandle<()>>,
}

#[derive(Clone)]
struct ExportState {
    shutdown: Arc<AtomicBool>,
    connected_streams: Arc<AtomicUsize>,
    connection_failures: Arc<AtomicU64>,
    authentication_failures: Arc<AtomicU64>,
    completed_sessions: Arc<AtomicU64>,
    active_streams: Arc<Mutex<Vec<Option<r9p_auth::SecureStream>>>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FilesystemExportStatus {
    pub connected_streams: usize,
    pub connection_failures: u64,
    pub authentication_failures: u64,
    pub completed_sessions: u64,
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
        let state = ExportState {
            shutdown: Arc::new(AtomicBool::new(false)),
            connected_streams: Arc::new(AtomicUsize::new(0)),
            connection_failures: Arc::new(AtomicU64::new(0)),
            authentication_failures: Arc::new(AtomicU64::new(0)),
            completed_sessions: Arc::new(AtomicU64::new(0)),
            active_streams: Arc::new(Mutex::new(
                std::iter::repeat_with(|| None)
                    .take(config.connection_pool)
                    .collect(),
            )),
        };
        let mut threads = Vec::with_capacity(config.connection_pool);
        for index in 0..config.connection_pool {
            let config = config.clone();
            let worker_state = state.clone();
            threads.push(
                thread::Builder::new()
                    .name(format!("r9p-reverse-export-{index}"))
                    .spawn(move || export_loop(config, index, worker_state))
                    .map_err(|error| Error::from(format!("spawn reverse export: {error}")))?,
            );
        }
        Ok(Self { state, threads })
    }

    pub fn connected_streams(&self) -> usize {
        self.state.connected_streams.load(Ordering::Acquire)
    }

    pub fn status(&self) -> FilesystemExportStatus {
        FilesystemExportStatus {
            connected_streams: self.state.connected_streams.load(Ordering::Acquire),
            connection_failures: self.state.connection_failures.load(Ordering::Acquire),
            authentication_failures: self.state.authentication_failures.load(Ordering::Acquire),
            completed_sessions: self.state.completed_sessions.load(Ordering::Acquire),
        }
    }
}

impl Drop for FilesystemExport {
    fn drop(&mut self) {
        if let Ok(streams) = self.state.active_streams.lock() {
            self.state.shutdown.store(true, Ordering::Release);
            for stream in streams.iter().flatten() {
                let _ = stream.shutdown();
            }
        } else {
            self.state.shutdown.store(true, Ordering::Release);
        }
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
        || config.reconnect_min_delay.is_zero()
        || config.reconnect_max_delay < config.reconnect_min_delay
        || config.msize < 1024
        || config.max_fids == 0
    {
        return Err(Error::from(
            "reverse export requires a broker endpoint, bounded pool, principal, finite timeouts, msize, and fid limit",
        ));
    }
    Ok(())
}

fn export_loop(config: FilesystemExportConfig, worker_index: usize, state: ExportState) {
    let mut failed_attempts = 0_u32;
    while !state.shutdown.load(Ordering::Acquire) {
        let stream =
            match TcpStream::connect_timeout(&config.broker_endpoint, config.connect_timeout) {
                Ok(stream) => stream,
                Err(_) => {
                    state.connection_failures.fetch_add(1, Ordering::AcqRel);
                    sleep_until_retry(
                        &state.shutdown,
                        retry_delay(&config, worker_index, failed_attempts),
                    );
                    failed_attempts = failed_attempts.saturating_add(1);
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
                state.authentication_failures.fetch_add(1, Ordering::AcqRel);
                sleep_until_retry(
                    &state.shutdown,
                    retry_delay(&config, worker_index, failed_attempts),
                );
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
        state.connected_streams.fetch_add(1, Ordering::AcqRel);
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
        if let Ok(mut streams) = state.active_streams.lock() {
            streams[worker_index] = None;
        }
        state.connected_streams.fetch_sub(1, Ordering::AcqRel);
        state.completed_sessions.fetch_add(1, Ordering::AcqRel);
    }
}

fn retry_delay(
    config: &FilesystemExportConfig,
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

fn sleep_until_retry(shutdown: &AtomicBool, duration: Duration) {
    let quantum = Duration::from_millis(25);
    let mut remaining = duration;
    while !shutdown.load(Ordering::Acquire) && !remaining.is_zero() {
        let delay = remaining.min(quantum);
        thread::sleep(delay);
        remaining = remaining.saturating_sub(delay);
    }
}
