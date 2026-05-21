//! `lookup` / `forget` op handlers.

use crate::{
    error::Result,
    fuse::{
        reply::{c_string, read_struct, reply_error, reply_struct},
        util::{is_namespace_shape_error, is_transport_error},
        wire::{FuseBatchForgetIn, FuseForgetIn, FuseForgetOne, FuseInHeader},
        R9pFuse,
    },
};
use std::{fs::File, mem::size_of};

impl R9pFuse {
    pub(in crate::fuse) fn lookup(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
        payload: &[u8],
    ) -> Result<()> {
        let name = c_string(payload)?.to_vec();
        if self.config.debug {
            eprintln!(
                "r9pfuse: lookup parent={} name={}",
                header.nodeid,
                String::from_utf8_lossy(&name)
            );
        }
        if name.contains(&b'/') {
            return reply_error(file, header.unique, libc::ENOENT);
        }
        match self.lookup_once(file, header, &name) {
            Ok(()) => Ok(()),
            Err(error) if is_transport_error(&error) => {
                self.reconnect()?;
                self.lookup_once(file, header, &name)
            }
            Err(error) if is_namespace_shape_error(&error) => {
                self.refresh_node(header.nodeid)?;
                self.lookup_once(file, header, &name)
            }
            Err(error) => Err(error),
        }
    }

    fn lookup_once(&mut self, file: &mut File, header: FuseInHeader, name: &[u8]) -> Result<()> {
        let (client, parent_fid) = self.bound_node_fid(header.nodeid)?;
        let fid = client.walk_one_timeout(parent_fid, name, self.config.request_timeout)?;
        let stat = client.stat_timeout(fid, self.config.request_timeout)?;
        let (nodeid, generation, clunk_fid) = {
            let mut nodes = self.nodes()?;
            let inserted = nodes.insert_lookup(header.nodeid, fid, stat.clone(), name)?;
            let nodeid = inserted.nodeid;
            let generation = nodes.node(nodeid)?.generation;
            (nodeid, generation, inserted.clunk_fid)
        };
        if self.config.debug {
            eprintln!("r9pfuse: lookup bound node {nodeid} with fid {fid}");
        }
        if let Some(clunk_fid) = clunk_fid {
            if self.config.debug {
                eprintln!("r9pfuse: lookup discarded fid {clunk_fid}");
            }
            let _ = client.clunk_timeout(clunk_fid, self.config.request_timeout);
        }
        let out = self.entry_out(nodeid, generation, &stat);
        reply_struct(file, header.unique, &out)
    }

    pub(in crate::fuse) fn forget(&mut self, header: FuseInHeader, payload: &[u8]) -> Result<()> {
        if let Ok(input) = read_struct::<FuseForgetIn>(payload) {
            self.forget_node(header.nodeid, input.nlookup)?;
        }
        Ok(())
    }

    pub(in crate::fuse) fn batch_forget(&mut self, payload: &[u8]) -> Result<()> {
        let Ok(input) = read_struct::<FuseBatchForgetIn>(payload) else {
            return Ok(());
        };
        let mut offset = size_of::<FuseBatchForgetIn>();
        for _ in 0..input.count {
            let Some(bytes) = payload.get(offset..) else {
                break;
            };
            let Ok(one) = read_struct::<FuseForgetOne>(bytes) else {
                break;
            };
            self.forget_node(one.nodeid, one.nlookup)?;
            offset = offset.saturating_add(size_of::<FuseForgetOne>());
        }
        Ok(())
    }

    fn forget_node(&mut self, nodeid: u64, nlookup: u64) -> Result<()> {
        let fid = self.nodes()?.forget(nodeid, nlookup);
        if let Some(fid) = fid {
            if let Ok(client) = self.client.snapshot() {
                let _ = client.clunk(fid);
            }
        }
        Ok(())
    }
}
