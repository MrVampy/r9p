use crate::{
    diagnostics::DEFAULT_DIAGNOSTICS_CAPACITY,
    error::{Error, Result},
};
use std::{path::PathBuf, time::Duration};

use super::change_feed;
pub const DEFAULT_MAX_WORKERS: usize = 10;
pub const DEFAULT_MAX_BACKGROUND: u16 = 12;
pub const DEFAULT_CHANGE_FEED_RECONNECT_DELAY: Duration = Duration::from_secs(1);
pub const DEFAULT_ATTR_TIMEOUT: Duration = Duration::from_secs(1);
pub const DEFAULT_ENTRY_TIMEOUT: Duration = Duration::from_secs(1);
pub const DEFAULT_NEGATIVE_TIMEOUT: Duration = Duration::ZERO;
pub const PERSISTENT_READ_CACHE_CHUNK_BYTES: u32 = 4 * 1024 * 1024;
pub const MAX_PERSISTENT_READ_CACHE_BYTES: u64 = PERSISTENT_READ_CACHE_CHUNK_BYTES as u64 * 65_536;

pub fn default_congestion_threshold(max_background: u16) -> u16 {
    ((u32::from(max_background) * 3 / 4).max(1)) as u16
}

#[derive(Debug, Clone)]
pub struct Config {
    pub address: String,
    pub fallback_addresses: Vec<String>,
    pub authentication: session::ConnectionAuthentication,
    pub source_path: String,
    pub mountpoint: String,
    pub uname: String,
    pub aname: String,
    pub msize: u32,
    pub connect_timeout: Duration,
    pub attr_timeout: Duration,
    pub entry_timeout: Duration,
    pub negative_timeout: Duration,
    pub request_timeout: Duration,
    pub lookup_timeout: Duration,
    pub read_timeout: Duration,
    pub change_feed_read_timeout: Duration,
    pub write_timeout: Duration,
    pub mutation_timeout: Duration,
    pub control_timeout: Duration,
    pub interrupt_timeout: Duration,
    pub max_workers: usize,
    pub max_background: u16,
    pub congestion_threshold: u16,
    pub diagnostics_path: Option<PathBuf>,
    pub diagnostics_capacity: usize,
    pub status_path: Option<PathBuf>,
    pub change_feed_path: Option<String>,
    pub change_feed_stream_path: Option<String>,
    pub change_feed_cursor_template: Option<String>,
    pub change_feed_scope: Option<String>,
    pub change_feed_reconnect_delay: Duration,
    pub change_feed_backpressure_limit: usize,
    pub coherent_read_cache: bool,
    pub read_cache_path: Option<PathBuf>,
    pub read_cache_max_bytes: u64,
    pub allow_other: bool,
    pub debug: bool,
}

impl Config {
    pub(super) fn connections(&self) -> Result<session::ConnectionSet> {
        let connection = |address: String| session::ConnectionConfig {
            address,
            uname: self.uname.clone(),
            aname: self.aname.clone(),
            msize: self.msize,
            authentication: self.authentication.clone(),
        };
        let mut candidates = Vec::with_capacity(1 + self.fallback_addresses.len());
        candidates.push(connection(self.address.clone()));
        candidates.extend(self.fallback_addresses.iter().cloned().map(connection));
        session::ConnectionSet::new(candidates).map_err(Error::from)
    }
}

pub(super) fn parse_source_path(path: &str) -> Result<Vec<Vec<u8>>> {
    if !path.starts_with('/') {
        return Err(Error::new(
            libc::EINVAL,
            format!("mount source path must be absolute: {path}"),
        ));
    }
    Ok(session::parse_namespace_path(path.as_bytes())?)
}

pub(super) fn normalize_config(config: &mut Config) -> Result<()> {
    if config.lookup_timeout.is_zero() {
        config.lookup_timeout = config.request_timeout;
    }
    if config.read_timeout.is_zero() {
        config.read_timeout = config.request_timeout;
    }
    if config.change_feed_read_timeout.is_zero() {
        config.change_feed_read_timeout = config.read_timeout;
    }
    if config.write_timeout.is_zero() {
        config.write_timeout = config.request_timeout;
    }
    if config.mutation_timeout.is_zero() {
        config.mutation_timeout = config.request_timeout;
    }
    if config.control_timeout.is_zero() {
        config.control_timeout = config.request_timeout;
    }
    if config.interrupt_timeout.is_zero() {
        config.interrupt_timeout = config.request_timeout.min(Duration::from_secs(1));
    }
    if config.max_workers == 0 {
        config.max_workers = DEFAULT_MAX_WORKERS;
    }
    if config.diagnostics_capacity == 0 {
        config.diagnostics_capacity = DEFAULT_DIAGNOSTICS_CAPACITY;
    }
    if config.change_feed_reconnect_delay.is_zero() {
        config.change_feed_reconnect_delay = DEFAULT_CHANGE_FEED_RECONNECT_DELAY;
    }
    if config.change_feed_backpressure_limit == 0 {
        config.change_feed_backpressure_limit = change_feed::DEFAULT_CHANGE_FEED_BACKPRESSURE_LIMIT;
    }
    if config.max_background == 0 {
        config.max_background = DEFAULT_MAX_BACKGROUND;
    }
    if config.congestion_threshold == 0 || config.congestion_threshold > config.max_background {
        config.congestion_threshold = default_congestion_threshold(config.max_background);
    }
    match (&config.read_cache_path, config.read_cache_max_bytes) {
        (None, 0) => {}
        (Some(path), max_bytes)
            if path.is_absolute()
                && max_bytes >= u64::from(PERSISTENT_READ_CACHE_CHUNK_BYTES)
                && max_bytes <= MAX_PERSISTENT_READ_CACHE_BYTES
                && config.coherent_read_cache => {}
        (Some(path), _) if !path.is_absolute() => {
            return Err(Error::new(
                libc::EINVAL,
                "persistent read cache path must be absolute",
            ));
        }
        (Some(_), max_bytes) if max_bytes < u64::from(PERSISTENT_READ_CACHE_CHUNK_BYTES) => {
            return Err(Error::new(
                libc::EINVAL,
                format!(
                    "persistent read cache quota must be at least {PERSISTENT_READ_CACHE_CHUNK_BYTES} bytes"
                ),
            ));
        }
        (Some(_), max_bytes) if max_bytes > MAX_PERSISTENT_READ_CACHE_BYTES => {
            return Err(Error::new(
                libc::EINVAL,
                format!(
                    "persistent read cache quota must not exceed {MAX_PERSISTENT_READ_CACHE_BYTES} bytes"
                ),
            ));
        }
        (Some(_), _) => {
            return Err(Error::new(
                libc::EINVAL,
                "persistent read cache requires coherent read caching",
            ));
        }
        (None, _) => {
            return Err(Error::new(
                libc::EINVAL,
                "persistent read cache quota requires a cache path",
            ));
        }
    }
    match (
        config.change_feed_path.as_ref(),
        config.change_feed_stream_path.as_ref(),
    ) {
        (None, None) if config.change_feed_cursor_template.is_none() => Ok(()),
        (Some(_), Some(_)) => Ok(()),
        _ => Err(Error::new(
            libc::EINVAL,
            "change feed requires both catch-up and blocking stream paths",
        )),
    }
}

pub(super) fn read_cache_volume_identity(config: &Config) -> Result<Vec<u8>> {
    let authentication = config.authentication.session().ok_or_else(|| {
        Error::new(
            libc::EINVAL,
            "persistent read cache requires an authenticated root",
        )
    })?;
    let responder = authentication.responder().ok_or_else(|| {
        Error::new(
            libc::EINVAL,
            "persistent read cache requires an authenticated root responder",
        )
    })?;
    let mut identity = Vec::new();
    for value in [
        config.source_path.as_bytes(),
        config.uname.as_bytes(),
        config.aname.as_bytes(),
        responder.as_str().as_bytes(),
    ] {
        let length = u64::try_from(value.len())
            .map_err(|_| Error::new(libc::EOVERFLOW, "read cache identity field too large"))?;
        identity.extend_from_slice(&length.to_le_bytes());
        identity.extend_from_slice(value);
    }
    Ok(identity)
}
