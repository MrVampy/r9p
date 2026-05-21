//! FUSE bridge over the standalone `r9p` 9P client.
//!
//! Submodules:
//! * [`wire`] — kernel-facing ABI structures and opcode constants.
//! * [`reply`] — reply framing and small byte helpers.
//! * [`mount`] — `/dev/fuse` acquisition via `fusermount`.
//! * [`util`] — stateless POSIX ↔ 9P conversion helpers.
//! * [`dispatch`] — event loop and opcode dispatch.
//! * [`ops`] — per-opcode handler implementations.

mod dispatch;
mod mount;
mod ops;
mod reply;
mod util;
mod wire;

use crate::{
    error::{Error, Result},
    node::{mode_kind, qid_to_inode, NodeTable},
    p9::Client,
};
use mount::mount_fuse;
use r9p::stat::Stat;
use std::{
    fs::File,
    os::fd::FromRawFd,
    path::Path,
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};
use util::duration_parts;
use wire::{FuseAttr, FuseAttrOut, FuseEntryOut};

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
    pub debug: bool,
}

#[derive(Clone)]
pub struct R9pFuse {
    client: ClientSlot,
    nodes: Arc<Mutex<NodeTable>>,
    config: Config,
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
    pub fn mount(config: Config) -> Result<()> {
        let client = Client::connect(&config.address, &config.uname, &config.aname, config.msize)?;
        let root_stat = client.stat(client.root_fid())?;
        let nodes = Arc::new(Mutex::new(NodeTable::new(client.root_fid(), root_stat)));
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };
        let fd = mount_fuse(Path::new(&config.mountpoint))?;
        let fs = Self {
            client: ClientSlot::new(client),
            nodes,
            config,
            uid,
            gid,
        };
        let mut file = unsafe { File::from_raw_fd(fd) };
        fs.run(&mut file)
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
                    eprintln!("r9pfuse: node {nodeid} uses cached fid {fid}");
                }
                Ok((client, fid))
            }
            _ => {
                let fid =
                    client.walk_timeout(client.root_fid(), &path, self.config.request_timeout)?;
                let stat = client.stat_timeout(fid, self.config.request_timeout)?;
                let old_fid = self.nodes()?.replace_binding(nodeid, fid, stat)?;
                if self.config.debug {
                    eprintln!("r9pfuse: node {nodeid} rebound to fid {fid}");
                }
                if let Some(old_fid) = old_fid {
                    let _ = client.clunk_timeout(old_fid, self.config.request_timeout);
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
    ) -> Result<Vec<r9p::fid::Fid>> {
        Ok(self.nodes()?.mark_path_bindings_stale())
    }

    pub(in crate::fuse) fn client_snapshot(&self) -> Result<Client> {
        self.client.snapshot()
    }

    pub(in crate::fuse) fn request_timeout(&self) -> Duration {
        self.config.request_timeout
    }

    fn attr(&self, stat: &Stat) -> FuseAttr {
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
}

#[cfg(test)]
mod tests {
    use super::ops::encode_dirents;
    use super::util::{
        flags_to_9p_mode, fuse_open_flags, is_namespace_shape_error, is_transport_error,
    };
    use super::wire::FOPEN_DIRECT_IO;
    use crate::error::Error;
    use crate::node::DirEntry;
    use crate::p9::{ORDWR, OREAD, OTRUNC, OWRITE};
    use r9p::{qid::Qid, stat::Stat};

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
}
