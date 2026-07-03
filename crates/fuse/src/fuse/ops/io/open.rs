use crate::{
    error::{Error, Result},
    fuse::{
        reply::{read_struct, reply_error, reply_struct},
        util::{flags_to_9p_mode, fuse_open_flags, is_namespace_shape_error, is_transport_error},
        wire::{FuseInHeader, FuseOpenIn, FuseOpenOut},
        R9pFuse,
    },
    node::{has_close_commit_mode, is_dir, read_open_directory_entries},
};
use session::{OREAD, OTRUNC};
use std::fs::File;

impl R9pFuse {
    pub(in crate::fuse) fn open(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
        payload: &[u8],
        is_dir_open: bool,
    ) -> Result<()> {
        let input = read_struct::<FuseOpenIn>(payload)?;
        let replayable = is_dir_open || flags_to_9p_mode(input.flags) == OREAD;
        match self.open_once(file, header, input, is_dir_open) {
            Ok(()) => Ok(()),
            Err(error) if is_namespace_shape_error(&error) => {
                self.recover_namespace_shape(header.nodeid)?;
                self.open_once(file, header, input, is_dir_open)
            }
            Err(error) if is_transport_error(&error) && replayable => {
                self.reconnect()?;
                self.open_once(file, header, input, is_dir_open)
            }
            Err(error) if is_transport_error(&error) => {
                self.reconnect()?;
                Err(Error::new(
                    libc::EIO,
                    "open failed during reconnect; mutating opens are not replayed",
                ))
            }
            Err(error) => Err(error),
        }
    }

    fn open_once(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
        input: FuseOpenIn,
        is_dir_open: bool,
    ) -> Result<()> {
        let node_stat = {
            let nodes = self.nodes()?;
            let node = nodes.node(header.nodeid)?;
            node.stat.clone()
        };
        if is_dir_open && !is_dir(&node_stat) {
            return reply_error(file, header.unique, libc::ENOTDIR);
        }
        let client = self.client_snapshot()?;
        if is_dir_open {
            if let Some(dir_entries) = self.cached_dir_entries_if_fresh(header.nodeid)? {
                let handle =
                    self.nodes()?
                        .open_handle(client, None, OREAD, true, false, false, dir_entries);
                let out = FuseOpenOut {
                    fh: handle,
                    open_flags: fuse_open_flags(true, OREAD),
                    padding: 0,
                };
                return reply_struct(file, header.unique, &out);
            }
        }
        let open_timeout = if flags_to_9p_mode(input.flags) == OREAD || is_dir_open {
            self.lookup_timeout()
        } else {
            self.mutation_timeout()
        };
        let fid = self.fresh_node_fid(header.nodeid, &client, open_timeout)?;
        let mut mode = flags_to_9p_mode(input.flags);
        if is_dir_open {
            mode = OREAD;
        }
        let close_commit_target = has_close_commit_mode(&node_stat);
        if close_commit_target {
            mode &= !OTRUNC;
        }
        if let Err(error) = client.open_timeout(fid, mode, open_timeout) {
            if mode & OTRUNC != 0
                && !is_transport_error(&error)
                && !is_namespace_shape_error(&error)
            {
                mode &= !OTRUNC;
                if let Err(error) = client.open_timeout(fid, mode, open_timeout) {
                    let _ = client.clunk_timeout(fid, self.control_timeout());
                    return Err(error.into());
                }
            } else {
                let _ = client.clunk_timeout(fid, self.control_timeout());
                return Err(error.into());
            }
        }
        let dir_entries = if is_dir_open {
            match read_open_directory_entries(&client, fid, self.control_timeout()) {
                Ok(entries) => {
                    self.nodes()?
                        .update_dir_cache(header.nodeid, entries.clone())?;
                    entries
                }
                Err(error) => {
                    let _ = client.clunk_timeout(fid, self.control_timeout());
                    return Err(error.into());
                }
            }
        } else {
            Vec::new()
        };
        let write_on_release = !is_dir_open && mode != OREAD;
        let close_commit = write_on_release && close_commit_target;
        let handle = self.nodes()?.open_handle(
            client.clone(),
            Some(fid),
            mode,
            is_dir_open,
            write_on_release,
            close_commit,
            dir_entries,
        );
        let out = FuseOpenOut {
            fh: handle,
            open_flags: fuse_open_flags(is_dir_open, mode),
            padding: 0,
        };
        reply_struct(file, header.unique, &out)
    }
}
