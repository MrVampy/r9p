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

use crate::{
    diagnostics::Diagnostics,
    error::{Error, Result},
    node::NodeTable,
};
use config::{normalize_config, parse_source_path};
use mount::{block_termination_signals, mount_fuse, FuseMount, MountCleanup};
use recovery::ShapeRecovery;
use session::{feed::FeedEventReceiver, ClientSession};
use status::MountStatus;
use std::{
    net::Shutdown,
    os::unix::net::UnixStream,
    path::Path,
    sync::{mpsc, Arc, Mutex},
    thread::{self, JoinHandle},
};

pub use config::{
    default_congestion_threshold, Config, DEFAULT_ATTR_TIMEOUT, DEFAULT_ENTRY_TIMEOUT,
    DEFAULT_MAX_BACKGROUND, DEFAULT_MAX_WORKERS, DEFAULT_NEGATIVE_TIMEOUT,
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
                false,
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
    uid: u32,
    gid: u32,
    shape_recovery: Arc<Mutex<ShapeRecovery>>,
    reconnect: Arc<Mutex<()>>,
}

impl R9pFuse {
    pub fn mount(mut config: Config) -> Result<()> {
        block_termination_signals();
        normalize_config(&mut config)?;
        let connections = config.connections()?;
        let client = ClientSession::connect_set(&connections, config.connect_timeout)?;
        Self::mount_prepared(config, client, None)
    }

    pub fn mount_with_session(
        mut config: Config,
        client: ClientSession,
        feed_events: Option<FeedEventReceiver>,
    ) -> Result<()> {
        block_termination_signals();
        normalize_config(&mut config)?;
        Self::mount_prepared(config, client, feed_events)
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
        let _ = ready.send(Ok(()));
        let cleanup = mount.cleanup_handle();
        fs.run_managed(mount.file_mut(), shutdown, cleanup)
    }

    fn mount_prepared(
        config: Config,
        client: ClientSession,
        feed_events: Option<FeedEventReceiver>,
    ) -> Result<()> {
        validate_coherent_cache(&config, feed_events.is_some())?;
        let mut mount = mount_fuse(
            Path::new(&config.mountpoint),
            &config.source_path,
            config.allow_other,
            true,
        )?;
        let fs = Self::prepare(config, client)?;
        fs.run(mount.file_mut(), feed_events)
    }

    fn prepare(config: Config, client: ClientSession) -> Result<Self> {
        let source_path = parse_source_path(&config.source_path)?;
        let diagnostics =
            Diagnostics::new(config.diagnostics_capacity, config.diagnostics_path.clone());
        let status = MountStatus::new(
            config.status_path.clone(),
            config.source_path.clone(),
            client.active_address(),
            client.candidate_addresses(),
        );
        let client_snapshot = client.snapshot()?;
        let _ = diagnostics.record(
            "mount_attached",
            0,
            0,
            0,
            0,
            format!(
                "source={} endpoint={} candidates={} msize={} max_write_payload={} fuse_max_write={}",
                config.source_path,
                client.active_address(),
                client.candidate_addresses().join(","),
                client_snapshot.msize(),
                client_snapshot.max_write_payload(),
                wire::DEFAULT_MAX_WRITE
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
            uid,
            gid,
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
