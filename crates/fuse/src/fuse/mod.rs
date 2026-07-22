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
mod config;
mod dispatch;
mod invalidation;
mod mount;
mod mount_state;
mod ops;
mod recovery;
mod reply;
mod status;
mod util;
mod wire;

#[cfg(test)]
mod tests;

use crate::{diagnostics::Diagnostics, error::Result, node::NodeTable};
use config::normalize_config;
use mount::{block_termination_signals, mount_fuse};
use recovery::ShapeRecovery;
use session::{feed::FeedEventReceiver, Client, ClientSlot};
use status::MountStatus;
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

pub use config::{
    default_congestion_threshold, Config, DEFAULT_ATTR_TIMEOUT, DEFAULT_ENTRY_TIMEOUT,
    DEFAULT_MAX_BACKGROUND, DEFAULT_MAX_WORKERS,
};

#[derive(Clone)]
pub struct R9pFuse {
    client: ClientSlot,
    nodes: Arc<Mutex<NodeTable>>,
    config: Config,
    diagnostics: Diagnostics,
    status: MountStatus,
    uid: u32,
    gid: u32,
    shape_recovery: Arc<Mutex<ShapeRecovery>>,
}

impl R9pFuse {
    pub fn mount(mut config: Config) -> Result<()> {
        block_termination_signals();
        normalize_config(&mut config);
        let client = Client::connect_with_timeout(&config.connection(), config.connect_timeout)?;
        Self::mount_prepared(config, ClientSlot::new(client), None)
    }

    pub fn mount_with_session(
        mut config: Config,
        client: ClientSlot,
        feed_events: Option<FeedEventReceiver>,
    ) -> Result<()> {
        block_termination_signals();
        normalize_config(&mut config);
        Self::mount_prepared(config, client, feed_events)
    }

    fn mount_prepared(
        config: Config,
        client: ClientSlot,
        feed_events: Option<FeedEventReceiver>,
    ) -> Result<()> {
        let diagnostics =
            Diagnostics::new(config.diagnostics_capacity, config.diagnostics_path.clone());
        let status = MountStatus::new(config.status_path.clone());
        let client_snapshot = client.snapshot()?;
        let _ = diagnostics.record(
            "mount_attached",
            0,
            0,
            0,
            0,
            format!(
                "msize={} max_write_payload={} fuse_max_write={}",
                client_snapshot.msize(),
                client_snapshot.max_write_payload(),
                wire::DEFAULT_MAX_WRITE
            ),
        );
        let root_stat =
            client_snapshot.stat_timeout(client_snapshot.root_fid(), config.lookup_timeout)?;
        let nodes = Arc::new(Mutex::new(NodeTable::new(
            client_snapshot.root_fid(),
            root_stat,
        )));
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };
        let mut mount = mount_fuse(Path::new(&config.mountpoint), config.allow_other)?;
        let fs = Self {
            client,
            nodes,
            config,
            diagnostics,
            status,
            uid,
            gid,
            shape_recovery: Arc::new(Mutex::new(ShapeRecovery::new())),
        };
        fs.run(mount.file_mut(), feed_events)
    }
}
