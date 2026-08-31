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
mod generation;
mod invalidation;
mod mount;
mod mount_state;
mod ops;
mod read_cache;
mod recovery;
mod reply;
mod status;
mod util;
mod wire;

#[cfg(test)]
mod tests;

use crate::{
    diagnostics::Diagnostics,
    error::{Error, Result},
    node::NodeTable,
};
use config::{normalize_config, parse_source_path, read_cache_volume_identity};
use generation::{block_mount_signals, notify_ready, MountGeneration};
use mount::{mount_fuse, FuseMount, MountCleanup};
use read_cache::ReadCache;
use recovery::ShapeRecovery;
use session::{feed::FeedEventReceiver, ClientSession};
use status::MountStatus;
use std::{
    io::{Read, Write},
    net::Shutdown,
    os::unix::net::UnixStream,
    path::Path,
    sync::{mpsc, Arc, Mutex},
    thread::{self, JoinHandle},
};

pub use config::{
    default_congestion_threshold, Config, DEFAULT_ATTR_TIMEOUT, DEFAULT_ENTRY_TIMEOUT,
    DEFAULT_MAX_BACKGROUND, DEFAULT_MAX_WORKERS, DEFAULT_NEGATIVE_TIMEOUT,
    MAX_PERSISTENT_READ_CACHE_BYTES, PERSISTENT_READ_CACHE_CHUNK_BYTES,
};

pub struct MountHandle {
    shutdown: Option<UnixStream>,
    join: Option<JoinHandle<()>>,
}

impl MountHandle {
    pub fn start(config: Config) -> Result<Self> {
        Self::start_all(vec![config])?
            .pop()
            .ok_or_else(|| Error::new(libc::EIO, "managed FUSE mount missing after startup"))
    }

    pub fn start_all(mut configs: Vec<Config>) -> Result<Vec<Self>> {
        let mut prepared = Vec::with_capacity(configs.len());
        for mut config in configs.drain(..) {
            normalize_config(&mut config)?;
            validate_coherent_cache(&config, false)?;
            parse_source_path(&config.source_path)?;
            let mount = mount_fuse(
                Path::new(&config.mountpoint),
                &config.source_path,
                config.allow_other,
            )?;
            prepared.push((config, mount));
        }

        let mut pending = Vec::with_capacity(prepared.len());
        for (config, mount) in prepared {
            let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
            let (shutdown, shutdown_wait) = UnixStream::pair()
                .map_err(|error| Error::io("create managed FUSE shutdown wake", error))?;
            let join = thread::Builder::new()
                .name("r9p-fuse-mount".to_string())
                .spawn(move || {
                    let result =
                        R9pFuse::mount_managed(config, mount, shutdown_wait, ready_sender.clone());
                    if let Err(error) = result {
                        let _ = ready_sender.send(Err((error.errno, error.message().to_string())));
                    }
                })
                .map_err(|error| Error::io("spawn managed FUSE mount", error))?;
            pending.push((
                Self {
                    shutdown: Some(shutdown),
                    join: Some(join),
                },
                ready_receiver,
            ));
        }

        let mut handles = Vec::with_capacity(pending.len());
        for (handle, ready) in pending {
            match ready.recv() {
                Ok(Ok(())) => handles.push(handle),
                Ok(Err((errno, message))) => return Err(Error::new(errno, message)),
                Err(_) => {
                    return Err(Error::new(
                        libc::EIO,
                        "managed FUSE mount stopped before readiness",
                    ));
                }
            }
        }
        Ok(handles)
    }

    pub fn stop(mut self) {
        self.stop_inner();
    }

    fn stop_inner(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.shutdown(Shutdown::Both);
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for MountHandle {
    fn drop(&mut self) {
        self.stop_inner();
    }
}

#[derive(Clone)]
pub struct R9pFuse {
    client: ClientSession,
    nodes: Arc<Mutex<NodeTable>>,
    source_path: Vec<Vec<u8>>,
    config: Config,
    diagnostics: Diagnostics,
    status: MountStatus,
    read_cache: Option<ReadCache>,
    uid: u32,
    gid: u32,
    max_request_bytes: u32,
    shape_recovery: Arc<Mutex<ShapeRecovery>>,
    reconnect: Arc<Mutex<()>>,
}

impl R9pFuse {
    pub fn mount(mut config: Config) -> Result<()> {
        block_mount_signals();
        normalize_config(&mut config)?;
        let connections = config.connections()?;
        let client = ClientSession::connect_set(&connections, config.connect_timeout)?;
        Self::mount_prepared(config, client, None, true, false)
    }

    pub fn mount_with_session(
        mut config: Config,
        client: ClientSession,
        feed_events: Option<FeedEventReceiver>,
    ) -> Result<()> {
        block_mount_signals();
        normalize_config(&mut config)?;
        Self::mount_prepared(config, client, feed_events, false, false)
    }

    pub fn mount_replacement(
        mut config: Config,
        mut prepared: UnixStream,
        mut start: UnixStream,
        mut ready: UnixStream,
    ) -> Result<()> {
        block_mount_signals();
        normalize_config(&mut config)?;
        let connections = config.connections()?;
        let client = ClientSession::connect_set(&connections, config.connect_timeout)?;
        let fs = Self::prepare(config, client)?;
        prepared
            .write_all(b"P")
            .map_err(|error| Error::io("publish replacement mount preflight", error))?;
        let mut command = [0_u8; 1];
        start
            .read_exact(&mut command)
            .map_err(|error| Error::io("wait for replacement mount start", error))?;
        if command != *b"G" {
            return Err(Error::new(
                libc::EPROTO,
                "replacement mount received an invalid start command",
            ));
        }
        let result = Self::mount_prepared_fs(fs, None, true, true, Some(&mut ready));
        if result.is_err() {
            let _ = ready.write_all(b"E");
        }
        result
    }

    fn mount_managed(
        config: Config,
        mut mount: FuseMount,
        shutdown: UnixStream,
        ready: mpsc::SyncSender<std::result::Result<(), (i32, String)>>,
    ) -> Result<()> {
        drop_managed_mount_thread_capabilities()?;
        let connections = config.connections()?;
        let client = ClientSession::connect_set(&connections, config.connect_timeout)?;
        let fs = Self::prepare(config, client)?;
        fs.status.activate();
        let _ = ready.send(Ok(()));
        let cleanup = mount.cleanup_handle();
        fs.run_managed(mount.file_mut(), shutdown, cleanup)
    }

    fn mount_prepared(
        config: Config,
        client: ClientSession,
        feed_events: Option<FeedEventReceiver>,
        notify_service: bool,
        adopt_main_process: bool,
    ) -> Result<()> {
        validate_coherent_cache(&config, feed_events.is_some())?;
        let fs = Self::prepare(config, client)?;
        Self::mount_prepared_fs(fs, feed_events, notify_service, adopt_main_process, None)
    }

    fn mount_prepared_fs(
        fs: Self,
        feed_events: Option<FeedEventReceiver>,
        notify_service: bool,
        adopt_main_process: bool,
        replacement_ready: Option<&mut UnixStream>,
    ) -> Result<()> {
        let mut mount = mount_fuse(
            Path::new(&fs.config.mountpoint),
            &fs.config.source_path,
            fs.config.allow_other,
        )?;
        let generation = MountGeneration::start(mount.cleanup_handle(), fs.status.clone())?;
        fs.status.activate();
        if notify_service {
            notify_ready(adopt_main_process)?;
        }
        if let Some(ready) = replacement_ready {
            ready
                .write_all(b"R")
                .map_err(|error| Error::io("publish replacement mount readiness", error))?;
        }
        let result = fs.run(mount.file_mut(), feed_events);
        generation.wait_for_successor_if_pending();
        result
    }

    fn prepare(config: Config, client: ClientSession) -> Result<Self> {
        let source_path = parse_source_path(&config.source_path)?;
        let read_cache = match config.read_cache_path.as_ref() {
            Some(path) => Some(ReadCache::open(
                path,
                config.read_cache_max_bytes,
                &read_cache_volume_identity(&config)?,
            )?),
            None => None,
        };
        let diagnostics =
            Diagnostics::new(config.diagnostics_capacity, config.diagnostics_path.clone());
        let status = MountStatus::new(
            config.status_path.clone(),
            config.source_path.clone(),
            client.active_address(),
            client.candidate_addresses(),
        );
        if let Some(cache) = read_cache.as_ref() {
            status.set_read_cache(cache.snapshot());
        }
        let client_snapshot = client.snapshot()?;
        let max_request_bytes = client_snapshot
            .max_write_payload()
            .min(wire::DEFAULT_MAX_IO_BYTES);
        let _ = diagnostics.record(
            "mount_attached",
            0,
            0,
            0,
            0,
            format!(
                "source={} endpoint={} candidates={} msize={} max_write_payload={} fuse_max_request_bytes={} persistent_read_cache={}",
                config.source_path,
                client.active_address(),
                client.candidate_addresses().join(","),
                client_snapshot.msize(),
                client_snapshot.max_write_payload(),
                max_request_bytes,
                if read_cache.is_some() { "enabled" } else { "disabled" }
            ),
        );
        let (root_fid, root_stat) =
            mount_state::source_binding(&client_snapshot, &source_path, config.lookup_timeout)?;
        let nodes = Arc::new(Mutex::new(NodeTable::new(root_fid, root_stat)));
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };
        Ok(Self {
            client,
            nodes,
            source_path,
            config,
            diagnostics,
            status,
            read_cache,
            uid,
            gid,
            max_request_bytes,
            shape_recovery: Arc::new(Mutex::new(ShapeRecovery::new())),
            reconnect: Arc::new(Mutex::new(())),
        })
    }
}

fn validate_coherent_cache(config: &Config, has_session_feed: bool) -> Result<()> {
    if config.coherent_read_cache && config.change_feed_path.is_none() && !has_session_feed {
        return Err(Error::new(
            libc::EINVAL,
            "coherent read cache requires a namespace change feed",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn drop_managed_mount_thread_capabilities() -> Result<()> {
    #[repr(C)]
    struct CapabilityHeader {
        version: u32,
        pid: i32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CapabilityData {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }

    if unsafe {
        libc::prctl(
            libc::PR_CAP_AMBIENT,
            libc::PR_CAP_AMBIENT_CLEAR_ALL,
            0,
            0,
            0,
        )
    } != 0
    {
        return Err(Error::io(
            "clear managed FUSE mount ambient capabilities",
            std::io::Error::last_os_error(),
        ));
    }
    let mut header = CapabilityHeader {
        version: 0x2008_0522,
        pid: 0,
    };
    let data = [
        CapabilityData {
            effective: 0,
            permitted: 0,
            inheritable: 0,
        },
        CapabilityData {
            effective: 0,
            permitted: 0,
            inheritable: 0,
        },
    ];
    let capset = unsafe {
        libc::syscall(
            libc::SYS_capset,
            (&mut header as *mut CapabilityHeader).cast::<libc::c_void>(),
            data.as_ptr().cast::<libc::c_void>(),
        )
    };
    if capset != 0 {
        return Err(Error::io(
            "drop managed FUSE mount capabilities",
            std::io::Error::last_os_error(),
        ));
    }
    let mut current = [
        CapabilityData {
            effective: u32::MAX,
            permitted: u32::MAX,
            inheritable: u32::MAX,
        },
        CapabilityData {
            effective: u32::MAX,
            permitted: u32::MAX,
            inheritable: u32::MAX,
        },
    ];
    let capget = unsafe {
        libc::syscall(
            libc::SYS_capget,
            (&mut header as *mut CapabilityHeader).cast::<libc::c_void>(),
            current.as_mut_ptr().cast::<libc::c_void>(),
        )
    };
    if capget != 0 {
        return Err(Error::io(
            "inspect managed FUSE mount capabilities",
            std::io::Error::last_os_error(),
        ));
    }
    if current
        .iter()
        .any(|entry| entry.effective != 0 || entry.permitted != 0 || entry.inheritable != 0)
    {
        return Err(Error::new(
            libc::EPERM,
            "managed FUSE mount capabilities remain after drop",
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn drop_managed_mount_thread_capabilities() -> Result<()> {
    Ok(())
}
