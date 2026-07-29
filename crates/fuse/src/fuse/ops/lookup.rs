//! `lookup` / `forget` op handlers.

use crate::{
    error::Result,
    fuse::{
        reply::{c_string, read_struct, reply_error, reply_struct},
        util::{is_lookup_namespace_shape_error, is_transport_error},
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
                "r9p mount: lookup parent={} name={}",
                header.nodeid,
                String::from_utf8_lossy(&name)
            );
        }
        if name.contains(&b'/') {
            return reply_error(file, header.unique, libc::ENOENT);
        }
        let result = match self.lookup_once(file, header, &name) {
            Ok(()) => Ok(()),
            Err(error) if is_transport_error(&error) => {
                self.reconnect()?;
                self.lookup_once(file, header, &name)
            }
            Err(error) if is_lookup_namespace_shape_error(&error) => {
                self.recover_namespace_shape(header.nodeid)?;
                self.lookup_once(file, header, &name)
            }
            Err(error) => Err(error),
        };
        match result {
            Err(error)
                if error.errno == libc::ENOENT && !self.config.negative_timeout.is_zero() =>
            {
                let out = self.negative_entry_out();
                reply_struct(file, header.unique, &out)
            }
            result => result,
        }
    }

    fn lookup_once(&mut self, file: &mut File, header: FuseInHeader, name: &[u8]) -> Result<()> {
        let client = self.client_snapshot()?;
        let fid = self.fresh_child_fid(header.nodeid, name, &client, self.lookup_timeout())?;
        let stat = client.stat_timeout(fid, self.lookup_timeout())?;
        let (nodeid, generation, cached_fid) = {
            let mut nodes = self.nodes()?;
            let nodeid = nodes.insert_lookup_lazy(header.nodeid, stat.clone(), name)?;
            let generation = nodes.node(nodeid)?.generation;
            let cached_fid = nodes.take_cached_fid(nodeid)?;
            (nodeid, generation, cached_fid)
        };
        if self.config.debug {
            eprintln!("r9p mount: lookup retained path-only node {nodeid}");
        }
        for clunk_fid in cached_fid.into_iter().chain(std::iter::once(fid)) {
            if self.config.debug {
                eprintln!("r9p mount: lookup released transient fid {clunk_fid}");
            }
            let _ = client.clunk_timeout(clunk_fid, self.control_timeout());
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
                let _ = client.clunk_timeout(fid, self.control_timeout());
            }
        }
        Ok(())
    }
}
