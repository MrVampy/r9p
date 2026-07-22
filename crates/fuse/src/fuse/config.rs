use crate::diagnostics::DEFAULT_DIAGNOSTICS_CAPACITY;
use std::{path::PathBuf, time::Duration};

use super::change_feed;

pub const DEFAULT_MAX_WORKERS: usize = 10;
pub const DEFAULT_MAX_BACKGROUND: u16 = 12;
pub const DEFAULT_CHANGE_FEED_POLL_INTERVAL: Duration = Duration::from_secs(1);
pub const DEFAULT_ATTR_TIMEOUT: Duration = Duration::from_secs(1);
pub const DEFAULT_ENTRY_TIMEOUT: Duration = Duration::from_secs(1);

pub fn default_congestion_threshold(max_background: u16) -> u16 {
    ((u32::from(max_background) * 3 / 4).max(1)) as u16
}

#[derive(Debug, Clone)]
pub struct Config {
    pub address: String,
    pub auth_config: Option<PathBuf>,
    pub mountpoint: String,
    pub uname: String,
    pub aname: String,
    pub msize: u32,
    pub connect_timeout: Duration,
    pub attr_timeout: Duration,
    pub entry_timeout: Duration,
    pub request_timeout: Duration,
    pub lookup_timeout: Duration,
    pub read_timeout: Duration,
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
    pub change_feed_poll_interval: Duration,
    pub change_feed_backpressure_limit: usize,
    pub allow_other: bool,
    pub debug: bool,
}

impl Config {
    pub(super) fn connection(&self) -> session::ConnectionConfig {
        session::ConnectionConfig {
            address: self.address.clone(),
            uname: self.uname.clone(),
            aname: self.aname.clone(),
            msize: self.msize,
            auth_config: self.auth_config.clone(),
        }
    }
}

pub(super) fn normalize_config(config: &mut Config) {
    if config.lookup_timeout.is_zero() {
        config.lookup_timeout = config.request_timeout;
    }
    if config.read_timeout.is_zero() {
        config.read_timeout = config.request_timeout;
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
    if config.change_feed_poll_interval.is_zero() {
        config.change_feed_poll_interval = DEFAULT_CHANGE_FEED_POLL_INTERVAL;
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
}
