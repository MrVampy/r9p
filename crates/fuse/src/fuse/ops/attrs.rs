//! `getattr` / `setattr` op handlers, including the Vault-specific truncate
//! fallback.

use crate::{
    error::{Error, Result},
    fuse::{
        reply::{read_struct, reply_struct},
        util::{is_namespace_shape_error, is_transport_error},
        wire::{
            FuseInHeader, FuseSetattrIn, FATTR_ATIME, FATTR_FH, FATTR_GID, FATTR_MODE, FATTR_MTIME,
            FATTR_SIZE, FATTR_UID,
        },
        R9pFuse,
    },
    node::null_wstat,
    p9::{Client, OTRUNC, OWRITE},
};
use r9p::fid::Fid;
use std::fs::File;

impl R9pFuse {
    pub(in crate::fuse) fn getattr(&mut self, file: &mut File, header: FuseInHeader) -> Result<()> {
        match self.getattr_once(file, header) {
            Ok(()) => Ok(()),
            Err(error) if is_transport_error(&error) => {
                self.reconnect()?;
                self.getattr_once(file, header)
            }
            Err(error) if is_namespace_shape_error(&error) => {
                self.refresh_node(header.nodeid)?;
                self.getattr_once(file, header)
            }
            Err(error) => Err(error),
        }
    }

    fn getattr_once(&mut self, file: &mut File, header: FuseInHeader) -> Result<()> {
        let cached = self.cached_node_stat_if_fresh(header.nodeid)?;
        if let Some(stat) = cached {
            return reply_struct(file, header.unique, &self.attr_out(&stat));
        };
        let (client, fid) = self.bound_node_fid(header.nodeid)?;
        let stat = client.stat_timeout(fid, self.lookup_timeout())?;
        self.nodes()?.update_stat(header.nodeid, stat.clone())?;
        let out = self.attr_out(&stat);
        reply_struct(file, header.unique, &out)
    }

    pub(in crate::fuse) fn setattr(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
        payload: &[u8],
    ) -> Result<()> {
        let input = read_struct::<FuseSetattrIn>(payload)?;
        let uses_handle = input.valid & FATTR_FH != 0;
        match self.setattr_once(file, header, input) {
            Ok(()) => Ok(()),
            Err(error) if is_namespace_shape_error(&error) && uses_handle => {
                self.refresh_node(header.nodeid)?;
                Err(Error::new(
                    libc::ESTALE,
                    "setattr file handle is stale after namespace refresh",
                ))
            }
            Err(error) if is_namespace_shape_error(&error) => {
                self.refresh_node(header.nodeid)?;
                self.setattr_once(file, header, input)
            }
            Err(error) => Err(error),
        }
    }

    fn setattr_once(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
        input: FuseSetattrIn,
    ) -> Result<()> {
        let (client, fid) = {
            let nodes = self.nodes()?;
            if input.valid & FATTR_FH != 0 {
                let handle = nodes.handle(input.fh)?;
                (handle.client.clone(), handle.fid)
            } else {
                drop(nodes);
                self.bound_node_fid(header.nodeid)?
            }
        };

        if input.valid & FATTR_SIZE != 0 {
            let mut stat = null_wstat();
            stat.length = input.size;
            if let Err(error) = client.wstat_timeout(fid, stat, self.mutation_timeout()) {
                if input.size == 0 {
                    self.truncate_fallback(&client, fid)?;
                } else {
                    return Err(error);
                }
            }
        }

        // Vault does not model Unix ownership, mode, atime, or mtime as
        // durable namespace state. Accept those FUSE setattr fields for editor
        // compatibility, but only translate size changes into 9P mutations.
        let _advisory_fields = input.valid & advisory_setattr_fields();
        let stat = client.stat_timeout(fid, self.lookup_timeout())?;
        let _ = self.nodes()?.update_stat(header.nodeid, stat.clone());
        let out = self.attr_out(&stat);
        reply_struct(file, header.unique, &out)
    }

    fn truncate_fallback(&mut self, client: &Client, fid: Fid) -> Result<()> {
        let timeout = self.mutation_timeout();
        let clone = client.clone_fid_timeout(fid, timeout)?;
        let open_result = client.open_timeout(clone, OWRITE | OTRUNC, timeout);
        if open_result.is_err() {
            if let Err(error) = client.open_timeout(clone, OWRITE, timeout) {
                let _ = client.clunk_timeout(clone, timeout);
                return Err(error);
            }
        }
        client.clunk_timeout(clone, timeout)
    }
}

fn advisory_setattr_fields() -> u32 {
    FATTR_UID | FATTR_GID | FATTR_MODE | FATTR_ATIME | FATTR_MTIME
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advisory_setattr_fields_exclude_size() {
        let advisory = advisory_setattr_fields();
        assert_eq!(advisory & FATTR_SIZE, 0);
        assert_ne!(advisory & FATTR_UID, 0);
        assert_ne!(advisory & FATTR_GID, 0);
        assert_ne!(advisory & FATTR_MODE, 0);
        assert_ne!(advisory & FATTR_ATIME, 0);
        assert_ne!(advisory & FATTR_MTIME, 0);
    }
}
