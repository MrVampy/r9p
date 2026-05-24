//! FUSE bridge over the standalone `r9p` 9P client.
//!
//! Submodules:
//! * [`wire`] — kernel-facing ABI structures and opcode constants.
//! * [`reply`] — reply framing and small byte helpers.
//! * [`mount`] — `/dev/fuse` acquisition via `fusermount`.
//! * [`util`] — stateless POSIX ↔ 9P conversion helpers.
//! * [`dispatch`] — event loop and opcode dispatch.
//! * [`ops`] — per-opcode handler implementations.

mod change_feed;
mod dispatch;
mod mount;
mod ops;
mod reply;
mod status;
mod util;
mod wire;

use crate::{
    diagnostics::{DiagnosticContext, DiagnosticRecord, Diagnostics, DEFAULT_DIAGNOSTICS_CAPACITY},
    error::{Error, Result},
    node::{mode_kind, qid_to_inode, NodeTable},
    p9::Client,
};
use mount::{block_termination_signals, mount_fuse};
use r9p::stat::Stat;
use status::MountStatus;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};
use util::duration_parts;
use wire::{FuseAttr, FuseAttrOut, FuseEntryOut};

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
    pub mountpoint: String,
    pub uname: String,
    pub aname: String,
    pub msize: u32,
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
    pub change_feed_scope: Option<String>,
    pub change_feed_poll_interval: Duration,
    pub change_feed_backpressure_limit: usize,
    pub debug: bool,
}

#[derive(Clone)]
pub struct R9pFuse {
    client: ClientSlot,
    nodes: Arc<Mutex<NodeTable>>,
    config: Config,
    diagnostics: Diagnostics,
    status: MountStatus,
    uid: u32,
    gid: u32,
}

#[derive(Clone)]
struct ClientSlot {
    current: Arc<RwLock<Client>>,
}

impl ClientSlot {
    fn new(client: Client) -> Self {
        Self {
            current: Arc::new(RwLock::new(client)),
        }
    }

    fn snapshot(&self) -> Result<Client> {
        self.current
            .read()
            .map_err(|_| Error::new(libc::EIO, "9P client lock poisoned"))
            .map(|client| client.clone())
    }

    fn replace(&self, client: Client) -> Result<()> {
        let mut current = self
            .current
            .write()
            .map_err(|_| Error::new(libc::EIO, "9P client lock poisoned"))?;
        *current = client;
        Ok(())
    }
}

impl R9pFuse {
    pub fn mount(mut config: Config) -> Result<()> {
        block_termination_signals();
        normalize_config(&mut config);
        let diagnostics =
            Diagnostics::new(config.diagnostics_capacity, config.diagnostics_path.clone());
        let status = MountStatus::new(config.status_path.clone());
        let client = Client::connect(&config.address, &config.uname, &config.aname, config.msize)?;
        let _ = diagnostics.record(
            "mount_attached",
            0,
            0,
            0,
            0,
            format!(
                "msize={} max_write_payload={} fuse_max_write={}",
                client.msize(),
                client.max_write_payload(),
                wire::DEFAULT_MAX_WRITE
            ),
        );
        let root_stat = client.stat_timeout(client.root_fid(), config.lookup_timeout)?;
        let nodes = Arc::new(Mutex::new(NodeTable::new(client.root_fid(), root_stat)));
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };
        let mut mount = mount_fuse(Path::new(&config.mountpoint))?;
        let fs = Self {
            client: ClientSlot::new(client),
            nodes,
            config,
            diagnostics,
            status,
            uid,
            gid,
        };
        fs.run(mount.file_mut())
    }

    pub(in crate::fuse) fn entry_out(
        &self,
        nodeid: u64,
        generation: u64,
        stat: &Stat,
    ) -> FuseEntryOut {
        let (entry_valid, entry_valid_nsec) = duration_parts(self.config.entry_timeout);
        let (attr_valid, attr_valid_nsec) = duration_parts(self.config.attr_timeout);
        FuseEntryOut {
            nodeid,
            generation,
            entry_valid,
            attr_valid,
            entry_valid_nsec,
            attr_valid_nsec,
            attr: self.attr(stat),
        }
    }

    pub(in crate::fuse) fn attr_out(&self, stat: &Stat) -> FuseAttrOut {
        let (attr_valid, attr_valid_nsec) = duration_parts(self.config.attr_timeout);
        FuseAttrOut {
            attr_valid,
            attr_valid_nsec,
            dummy: 0,
            attr: self.attr(stat),
        }
    }

    pub(in crate::fuse) fn bound_node_fid(
        &mut self,
        nodeid: u64,
    ) -> Result<(Client, r9p::fid::Fid)> {
        let (path, existing, needs_rebind) = {
            let nodes = self.nodes()?;
            let node = nodes.node(nodeid)?;
            (node.path.clone(), node.fid, node.needs_rebind)
        };
        let client = self.client.snapshot()?;
        match (existing, needs_rebind) {
            (Some(fid), false) => {
                if self.config.debug {
                    eprintln!("r9p mount: node {nodeid} uses cached fid {fid}");
                }
                Ok((client, fid))
            }
            _ => {
                let fid = client.walk_timeout(client.root_fid(), &path, self.lookup_timeout())?;
                let stat = client.stat_timeout(fid, self.lookup_timeout())?;
                let old_fid = self.nodes()?.replace_binding(nodeid, fid, stat)?;
                if self.config.debug {
                    eprintln!("r9p mount: node {nodeid} rebound to fid {fid}");
                }
                if let Some(old_fid) = old_fid {
                    let _ = client.clunk_timeout(old_fid, self.control_timeout());
                }
                Ok((client, fid))
            }
        }
    }

    pub(in crate::fuse) fn cached_node_stat_if_fresh(&self, nodeid: u64) -> Result<Option<Stat>> {
        let nodes = self.nodes()?;
        let node = nodes.node(nodeid)?;
        match (node.fid, node.needs_rebind) {
            (None, false) => Ok(Some(node.stat.clone())),
            _ => Ok(None),
        }
    }

    pub(in crate::fuse) fn refresh_path_bindings_after_namespace_change(
        &mut self,
    ) -> Result<Vec<crate::node::StaleBinding>> {
        Ok(self.nodes()?.mark_path_bindings_stale())
    }

    pub(in crate::fuse) fn client_snapshot(&self) -> Result<Client> {
        self.client.snapshot()
    }

    pub(in crate::fuse) fn lookup_timeout(&self) -> Duration {
        self.config.lookup_timeout
    }

    pub(in crate::fuse) fn read_timeout(&self) -> Duration {
        self.config.read_timeout
    }

    pub(in crate::fuse) fn write_timeout(&self) -> Duration {
        self.config.write_timeout
    }

    pub(in crate::fuse) fn mutation_timeout(&self) -> Duration {
        self.config.mutation_timeout
    }

    pub(in crate::fuse) fn control_timeout(&self) -> Duration {
        self.config.control_timeout
    }

    pub(in crate::fuse) fn interrupt_timeout(&self) -> Duration {
        self.config.interrupt_timeout
    }

    pub(in crate::fuse) fn record_diagnostic(
        &self,
        event: &'static str,
        header: wire::FuseInHeader,
        errno: i32,
        message: impl Into<String>,
    ) {
        let context = self.diagnostic_context(header, &[]);
        self.record_diagnostic_with_context(event, header, errno, message, context);
    }

    pub(in crate::fuse) fn record_diagnostic_with_context(
        &self,
        event: &'static str,
        header: wire::FuseInHeader,
        errno: i32,
        message: impl Into<String>,
        context: DiagnosticContext,
    ) {
        let _ = self.diagnostics.record_entry(DiagnosticRecord {
            event,
            opcode: header.opcode,
            unique: header.unique,
            nodeid: header.nodeid,
            errno,
            message: message.into(),
            context,
        });
    }

    pub(in crate::fuse) fn record_mount_diagnostic(
        &self,
        event: &'static str,
        errno: i32,
        message: impl Into<String>,
    ) {
        let _ = self.diagnostics.record(event, 0, 0, 0, errno, message);
    }

    pub(in crate::fuse) fn diagnostic_context(
        &self,
        header: wire::FuseInHeader,
        payload: &[u8],
    ) -> DiagnosticContext {
        let mut context = DiagnosticContext {
            path: self.path_for_nodeid(header.nodeid),
            ..DiagnosticContext::default()
        };
        match header.opcode {
            wire::FUSE_READ | wire::FUSE_READDIR | wire::FUSE_READDIRPLUS => {
                if let Ok(input) = reply::read_struct::<wire::FuseReadIn>(payload) {
                    context.fh = Some(input.fh);
                    context.offset = Some(input.offset);
                    context.size = Some(u64::from(input.size));
                }
            }
            wire::FUSE_WRITE => {
                if let Ok(input) = reply::read_struct::<wire::FuseWriteIn>(payload) {
                    context.fh = Some(input.fh);
                    context.offset = Some(input.offset);
                    context.size = Some(u64::from(input.size));
                }
            }
            wire::FUSE_RELEASE | wire::FUSE_RELEASEDIR => {
                if let Ok(input) = reply::read_struct::<wire::FuseReleaseIn>(payload) {
                    context.fh = Some(input.fh);
                }
            }
            wire::FUSE_FLUSH => {
                if let Ok(input) = reply::read_struct::<wire::FuseFlushIn>(payload) {
                    context.fh = Some(input.fh);
                }
            }
            wire::FUSE_FSYNC | wire::FUSE_FSYNCDIR => {
                if let Ok(input) = reply::read_struct::<wire::FuseFsyncIn>(payload) {
                    context.fh = Some(input.fh);
                }
            }
            wire::FUSE_SETATTR => {
                if let Ok(input) = reply::read_struct::<wire::FuseSetattrIn>(payload) {
                    if input.valid & wire::FATTR_FH != 0 {
                        context.fh = Some(input.fh);
                    }
                    if input.valid & wire::FATTR_SIZE != 0 {
                        context.size = Some(input.size);
                    }
                }
            }
            wire::FUSE_POLL => {
                if let Ok(input) = reply::read_struct::<wire::FusePollIn>(payload) {
                    context.fh = Some(input.fh);
                }
            }
            _ => {}
        }
        context
    }

    pub(in crate::fuse) fn attr(&self, stat: &Stat) -> FuseAttr {
        FuseAttr {
            ino: qid_to_inode(stat.qid),
            size: stat.length,
            blocks: stat.length.saturating_add(8191) / 8192,
            atime: u64::from(stat.atime),
            mtime: u64::from(stat.mtime),
            ctime: u64::from(stat.mtime),
            atimensec: 0,
            mtimensec: 0,
            ctimensec: 0,
            mode: mode_kind(stat) | (stat.mode & 0o777),
            nlink: 1,
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 8192,
            flags: 0,
        }
    }

    fn path_for_nodeid(&self, nodeid: u64) -> Option<String> {
        let nodes = self.nodes().ok()?;
        let node = nodes.node(nodeid).ok()?;
        Some(format_namespace_path(&node.path))
    }
}

fn format_namespace_path(path: &[Vec<u8>]) -> String {
    if path.is_empty() {
        return "/".to_string();
    }
    let mut out = String::new();
    for segment in path {
        out.push('/');
        out.push_str(&String::from_utf8_lossy(segment));
    }
    out
}

fn normalize_config(config: &mut Config) {
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

#[cfg(test)]
mod tests {
    use super::ops::encode_dirents;
    use super::util::{
        flags_to_9p_mode, fuse_open_flags, is_namespace_shape_error, is_transport_error,
    };
    use super::wire::FOPEN_DIRECT_IO;
    use super::{
        change_feed, default_congestion_threshold, normalize_config, Config,
        DEFAULT_CHANGE_FEED_POLL_INTERVAL, DEFAULT_MAX_BACKGROUND, DEFAULT_MAX_WORKERS,
    };
    use crate::error::Error;
    use crate::node::DirEntry;
    use crate::p9::{ORDWR, OREAD, OTRUNC, OWRITE};
    use r9p::{qid::Qid, stat::Stat};
    use std::time::Duration;

    #[test]
    fn maps_truncating_write_flags_to_9p_mode() {
        let flags = libc::O_WRONLY as u32 | libc::O_TRUNC as u32;
        assert_eq!(flags_to_9p_mode(flags), OWRITE | OTRUNC);
    }

    #[test]
    fn maps_read_only_flags_to_9p_read() {
        assert_eq!(flags_to_9p_mode(libc::O_RDONLY as u32), OREAD);
    }

    #[test]
    fn directory_encoding_respects_buffer_size() {
        let entry = DirEntry {
            name: b"alpha".to_vec(),
            qid: Qid::file(7),
            stat: Stat::new("alpha", Qid::file(7), 0o444),
        };
        let bytes = encode_dirents(1, 0, 1024, &[entry]).expect("dirents should encode");
        assert!(!bytes.is_empty());
        let too_small = encode_dirents(1, 0, 1, &[]).expect("dirents should encode");
        assert!(too_small.is_empty());
    }

    #[test]
    fn read_only_file_opens_use_direct_io_for_unknown_size_reads() {
        assert_eq!(fuse_open_flags(false, OREAD), FOPEN_DIRECT_IO);
        assert_eq!(fuse_open_flags(false, OWRITE), 0);
        assert_eq!(fuse_open_flags(false, ORDWR), 0);
        assert_eq!(fuse_open_flags(false, OWRITE | OTRUNC), 0);
        assert_eq!(fuse_open_flags(true, OREAD), 0);
    }

    #[test]
    fn namespace_shape_errors_are_reconnect_candidates() {
        assert!(is_namespace_shape_error(&Error::new(
            libc::ENOENT,
            "walk failed after namespace reload",
        )));
        assert!(is_namespace_shape_error(&Error::new(
            libc::ESTALE,
            "unknown fid",
        )));
        assert!(!is_namespace_shape_error(&Error::new(
            libc::EACCES,
            "permission denied",
        )));
        assert!(!is_namespace_shape_error(&Error::new(
            libc::ESTALE,
            "application-level stale value",
        )));
    }

    #[test]
    fn closed_9p_reader_errors_are_reconnect_candidates() {
        assert!(is_transport_error(&Error::new(
            libc::ENOTCONN,
            "9P client state: 9P reader stopped before response",
        )));
        assert!(is_transport_error(&Error::new(
            libc::EIO,
            "9P client state: 9P reader stopped before response",
        )));
        assert!(!is_transport_error(&Error::new(
            libc::EPROTO,
            "9P client state: response tag mismatch",
        )));
    }

    #[test]
    fn default_congestion_threshold_matches_kernel_ratio() {
        assert_eq!(default_congestion_threshold(12), 9);
        assert_eq!(default_congestion_threshold(1), 1);
    }

    #[test]
    fn mount_config_normalization_keeps_worker_and_background_limits_nonzero() {
        let mut config = Config {
            address: "127.0.0.1:564".to_string(),
            mountpoint: "/tmp/r9p-mount".to_string(),
            uname: "codex".to_string(),
            aname: "/".to_string(),
            msize: 8192,
            attr_timeout: Duration::ZERO,
            entry_timeout: Duration::ZERO,
            request_timeout: Duration::from_secs(5),
            lookup_timeout: Duration::ZERO,
            read_timeout: Duration::ZERO,
            write_timeout: Duration::ZERO,
            mutation_timeout: Duration::ZERO,
            control_timeout: Duration::ZERO,
            interrupt_timeout: Duration::ZERO,
            max_workers: 0,
            max_background: 0,
            congestion_threshold: 99,
            diagnostics_path: None,
            diagnostics_capacity: 0,
            status_path: None,
            change_feed_path: None,
            change_feed_scope: None,
            change_feed_poll_interval: Duration::ZERO,
            change_feed_backpressure_limit: 0,
            debug: false,
        };

        normalize_config(&mut config);

        assert_eq!(config.lookup_timeout, Duration::from_secs(5));
        assert_eq!(config.interrupt_timeout, Duration::from_secs(1));
        assert_eq!(config.max_workers, DEFAULT_MAX_WORKERS);
        assert_eq!(
            config.change_feed_poll_interval,
            DEFAULT_CHANGE_FEED_POLL_INTERVAL
        );
        assert_eq!(
            config.change_feed_backpressure_limit,
            change_feed::DEFAULT_CHANGE_FEED_BACKPRESSURE_LIMIT
        );
        assert_eq!(config.max_background, DEFAULT_MAX_BACKGROUND);
        assert_eq!(
            config.congestion_threshold,
            default_congestion_threshold(DEFAULT_MAX_BACKGROUND)
        );
    }
}
