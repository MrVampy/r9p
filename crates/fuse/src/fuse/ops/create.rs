//! `create` / `mkdir` / `mknod` op handlers.

use crate::{
    error::{Error, Result},
    fuse::{
        reply::{c_string, read_struct, reply_struct},
        util::{flags_to_9p_mode, fuse_open_flags, is_namespace_shape_error},
        wire::{FuseCreateIn, FuseCreateOut, FuseInHeader, FuseMkdirIn, FuseMknodIn, FuseOpenOut},
        R9pFuse,
    },
    p9::OREAD,
};
use r9p::{
    fid::Fid,
    qid::{Qid, DMDIR},
    stat::Stat,
};
use std::{fs::File, mem::size_of};

impl R9pFuse {
    pub(in crate::fuse) fn create(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
        payload: &[u8],
    ) -> Result<()> {
        let input = read_struct::<FuseCreateIn>(payload)?;
        let name = c_string(
            payload
                .get(size_of::<FuseCreateIn>()..)
                .ok_or_else(|| Error::new(libc::EINVAL, "missing create name"))?,
        )?;
        let mode = flags_to_9p_mode(input.flags);
        let perm = input.mode & 0o777;
        let (client, parent_fid, open_fid, created_qid) =
            match self.create_remote(header.nodeid, name, perm, mode) {
                Ok(created) => created,
                Err(error) if is_namespace_shape_error(&error) => {
                    self.refresh_node(header.nodeid)?;
                    self.create_remote(header.nodeid, name, perm, mode)?
                }
                Err(error) => return Err(error),
            };
        let walked = match client.walk_one_timeout(parent_fid, name, self.lookup_timeout()) {
            Ok(node_fid) => match client.stat_timeout(node_fid, self.lookup_timeout()) {
                Ok(stat) => Ok((node_fid, stat)),
                Err(error) => {
                    let _ = client.clunk_timeout(node_fid, self.control_timeout());
                    Err(error)
                }
            },
            Err(error) => Err(error),
        };
        let (nodeid, generation, handle, stat, clunk_fid) = match walked {
            Ok((node_fid, stat)) => {
                let mut nodes = self.nodes()?;
                let inserted = nodes.insert_lookup(header.nodeid, node_fid, stat.clone(), name)?;
                let nodeid = inserted.nodeid;
                let handle = nodes.open_handle(client.clone(), open_fid, false, Vec::new());
                let generation = nodes.node(nodeid)?.generation;
                (nodeid, generation, handle, stat, inserted.clunk_fid)
            }
            Err(error) if is_namespace_shape_error(&error) => {
                let stat = synthetic_created_stat(name, created_qid, perm);
                let mut nodes = self.nodes()?;
                let nodeid = nodes.insert_lookup_lazy(header.nodeid, stat.clone(), name)?;
                let handle = nodes.open_handle(client.clone(), open_fid, false, Vec::new());
                let generation = nodes.node(nodeid)?.generation;
                (nodeid, generation, handle, stat, None)
            }
            Err(error) => {
                let _ = client.clunk_timeout(open_fid, self.control_timeout());
                return Err(error);
            }
        };
        if let Some(clunk_fid) = clunk_fid {
            let _ = client.clunk_timeout(clunk_fid, self.control_timeout());
        }
        let out = FuseCreateOut {
            entry: self.entry_out(nodeid, generation, &stat),
            open: FuseOpenOut {
                fh: handle,
                open_flags: fuse_open_flags(false, mode),
                padding: 0,
            },
        };
        reply_struct(file, header.unique, &out)
    }

    pub(in crate::fuse) fn mkdir(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
        payload: &[u8],
    ) -> Result<()> {
        let input = read_struct::<FuseMkdirIn>(payload)?;
        let name = c_string(
            payload
                .get(size_of::<FuseMkdirIn>()..)
                .ok_or_else(|| Error::new(libc::EINVAL, "missing mkdir name"))?,
        )?;
        let (client, parent_fid, fid, _) =
            match self.create_remote(header.nodeid, name, DMDIR | (input.mode & 0o777), OREAD) {
                Ok(created) => created,
                Err(error) if is_namespace_shape_error(&error) => {
                    self.refresh_node(header.nodeid)?;
                    self.create_remote(header.nodeid, name, DMDIR | (input.mode & 0o777), OREAD)?
                }
                Err(error) => return Err(error),
            };
        let _ = client.clunk_timeout(fid, self.control_timeout());
        self.insert_created_node(file, header, &client, parent_fid, name)
    }

    pub(in crate::fuse) fn mknod(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
        payload: &[u8],
    ) -> Result<()> {
        let input = read_struct::<FuseMknodIn>(payload)?;
        let name = c_string(
            payload
                .get(size_of::<FuseMknodIn>()..)
                .ok_or_else(|| Error::new(libc::EINVAL, "missing mknod name"))?,
        )?;
        let (client, parent_fid, fid, _) =
            match self.create_remote(header.nodeid, name, input.mode & 0o777, OREAD) {
                Ok(created) => created,
                Err(error) if is_namespace_shape_error(&error) => {
                    self.refresh_node(header.nodeid)?;
                    self.create_remote(header.nodeid, name, input.mode & 0o777, OREAD)?
                }
                Err(error) => return Err(error),
            };
        let _ = client.clunk_timeout(fid, self.control_timeout());
        self.insert_created_node(file, header, &client, parent_fid, name)
    }

    fn create_remote(
        &mut self,
        parent_nodeid: u64,
        name: &[u8],
        perm: u32,
        mode: u8,
    ) -> Result<(crate::p9::Client, Fid, Fid, Qid)> {
        // Create-family operations only retry around the initial Tcreate. Once
        // the server reports that creation succeeded, follow-up walks/stats are
        // not replayed as a second create. That preserves the Plan 37 contract:
        // path-backed state may be rebound, but mutating operations are not
        // duplicated after an ambiguous partial success.
        let (client, parent_fid) = self.bound_node_fid(parent_nodeid)?;
        let timeout = self.mutation_timeout();
        let create_fid = client.clone_fid_timeout(parent_fid, timeout)?;
        let (fid, qid) = match client.create_timeout(create_fid, name, perm, mode, timeout) {
            Ok(created) => created,
            Err(error) => {
                let _ = client.clunk_timeout(create_fid, self.control_timeout());
                return Err(error);
            }
        };
        Ok((client, parent_fid, fid, qid))
    }

    fn insert_created_node(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
        client: &crate::p9::Client,
        parent_fid: Fid,
        name: &[u8],
    ) -> Result<()> {
        let node_fid = client.walk_one_timeout(parent_fid, name, self.lookup_timeout())?;
        let stat = client.stat_timeout(node_fid, self.lookup_timeout())?;
        let (nodeid, generation, clunk_fid) = {
            let mut nodes = self.nodes()?;
            let inserted = nodes.insert_lookup(header.nodeid, node_fid, stat.clone(), name)?;
            let nodeid = inserted.nodeid;
            let generation = nodes.node(nodeid)?.generation;
            (nodeid, generation, inserted.clunk_fid)
        };
        if let Some(clunk_fid) = clunk_fid {
            let _ = client.clunk_timeout(clunk_fid, self.control_timeout());
        }
        let out = self.entry_out(nodeid, generation, &stat);
        reply_struct(file, header.unique, &out)
    }
}

fn synthetic_created_stat(name: &[u8], qid: Qid, perm: u32) -> Stat {
    Stat::new(name.to_vec(), qid, perm & 0o777)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_created_stat_uses_create_qid_and_file_mode() {
        let qid = Qid::file(42);
        let stat = synthetic_created_stat(b"pending", qid, 0o100644);
        assert_eq!(stat.name, b"pending".to_vec());
        assert_eq!(stat.qid, qid);
        assert_eq!(stat.mode, 0o644);
        assert_eq!(stat.length, 0);
    }
}
