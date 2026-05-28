//! `open` / `read` / `write` / `release` op handlers.

use super::namespace_change::{
    is_runtime_control_write_path, refreshes_namespace_bindings, write_refreshes_namespace_bindings,
};
use crate::{
    error::{Error, Result},
    fuse::{
        reply::{read_struct, reply_bytes, reply_empty, reply_error, reply_struct},
        util::{flags_to_9p_mode, fuse_open_flags, is_namespace_shape_error, is_transport_error},
        wire::{
            FuseInHeader, FuseOpenIn, FuseOpenOut, FuseReadIn, FuseReleaseIn, FuseWriteIn,
            FuseWriteOut,
        },
        R9pFuse,
    },
    node::{is_dir, is_symlink, read_directory_entries, CLOSE_COMMIT_MODE_FLAG},
    p9::{OREAD, OTRUNC},
};
use std::{fs::File, mem::size_of};

impl R9pFuse {
    pub(in crate::fuse) fn readlink(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
    ) -> Result<()> {
        let stat = {
            let nodes = self.nodes()?;
            nodes.node(header.nodeid)?.stat.clone()
        };
        if !is_symlink(&stat) {
            return reply_error(file, header.unique, libc::EINVAL);
        }
        let (client, fid) = self.bound_node_fid(header.nodeid)?;
        let count = symlink_read_count(&stat)?;
        let data = match client.read_full_timeout(fid, 0, count, self.read_timeout()) {
            Ok(data) => data,
            Err(error) if is_transport_error(&error) => {
                self.reconnect()?;
                return Err(Error::new(
                    libc::ESTALE,
                    "symlink handle is stale after 9P reconnect",
                ));
            }
            Err(error) if is_namespace_shape_error(&error) => {
                self.refresh_node(header.nodeid)?;
                return Err(Error::new(
                    libc::ESTALE,
                    "symlink handle is stale after namespace refresh",
                ));
            }
            Err(error) => return Err(error),
        };
        reply_bytes(file, header.unique, &data)
    }

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
                self.refresh_node(header.nodeid)?;
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
        let (client, node_fid) = self.bound_node_fid(header.nodeid)?;
        let open_timeout = if flags_to_9p_mode(input.flags) == OREAD || is_dir_open {
            self.lookup_timeout()
        } else {
            self.mutation_timeout()
        };
        let fid = client.clone_fid_timeout(node_fid, open_timeout)?;
        let mut mode = flags_to_9p_mode(input.flags);
        if is_dir_open {
            mode = OREAD;
        }
        if let Err(error) = client.open_timeout(fid, mode, open_timeout) {
            if mode & OTRUNC != 0
                && !is_transport_error(&error)
                && !is_namespace_shape_error(&error)
            {
                mode &= !OTRUNC;
                if let Err(error) = client.open_timeout(fid, mode, open_timeout) {
                    let _ = client.clunk_timeout(fid, self.control_timeout());
                    return Err(error);
                }
            } else {
                let _ = client.clunk_timeout(fid, self.control_timeout());
                return Err(error);
            }
        }
        let dir_entries = if is_dir_open {
            let mut dir_client = client.clone();
            match read_directory_entries(&mut dir_client, node_fid, self.control_timeout()) {
                Ok(entries) => entries,
                Err(error) => {
                    let _ = client.clunk_timeout(fid, self.control_timeout());
                    return Err(error);
                }
            }
        } else {
            Vec::new()
        };
        let write_on_release = !is_dir_open && mode != OREAD;
        let close_commit = write_on_release && node_stat.mode & CLOSE_COMMIT_MODE_FLAG != 0;
        let handle = self.nodes()?.open_handle(
            client.clone(),
            fid,
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

    pub(in crate::fuse) fn read(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
        payload: &[u8],
    ) -> Result<()> {
        let input = read_struct::<FuseReadIn>(payload)?;
        let known_length = {
            let nodes = self.nodes()?;
            nodes.node(header.nodeid)?.stat.length
        };
        if read_is_known_eof(known_length, input.offset) {
            return reply_bytes(file, header.unique, &[]);
        }
        let handle = self.nodes()?.handle(input.fh)?.clone();
        let data = match handle.client.read_timeout(
            handle.fid,
            input.offset,
            input.size,
            self.read_timeout(),
        ) {
            Ok(data) => data,
            Err(error) if is_transport_error(&error) => {
                self.reconnect()?;
                return Err(Error::new(
                    libc::ESTALE,
                    "file handle is stale after 9P reconnect",
                ));
            }
            Err(error) if is_namespace_shape_error(&error) => {
                self.refresh_node(header.nodeid)?;
                return Err(Error::new(
                    libc::ESTALE,
                    "file handle is stale after namespace refresh",
                ));
            }
            Err(error) => return Err(error),
        };
        reply_bytes(file, header.unique, &data)
    }

    pub(in crate::fuse) fn write(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
        payload: &[u8],
    ) -> Result<()> {
        let input = read_struct::<FuseWriteIn>(payload)?;
        let header_len = size_of::<FuseWriteIn>();
        let size =
            usize::try_from(input.size).map_err(|_| Error::new(libc::EINVAL, "write too large"))?;
        let data = payload
            .get(header_len..header_len.saturating_add(size))
            .ok_or_else(|| Error::new(libc::EINVAL, "truncated FUSE write"))?;
        let node_path = {
            let nodes = self.nodes()?;
            nodes.node(header.nodeid)?.path.clone()
        };
        let handle = self.nodes()?.handle(input.fh)?.clone();
        let write_timeout = if is_runtime_control_write_path(&node_path) {
            self.control_timeout()
        } else {
            self.write_timeout()
        };
        let count = match handle
            .client
            .write_timeout(handle.fid, input.offset, data, write_timeout)
        {
            Ok(count) => count,
            Err(error) if is_transport_error(&error) => {
                self.reconnect()?;
                return Err(Error::new(
                    libc::EIO,
                    "write failed during reconnect and was not replayed",
                ));
            }
            Err(error) if is_namespace_shape_error(&error) => {
                self.refresh_node(header.nodeid)?;
                return Err(Error::new(
                    libc::EIO,
                    "write failed during namespace refresh and was not replayed",
                ));
            }
            Err(error) => return Err(error),
        };
        let invalidate_after_reply =
            write_refreshes_namespace_bindings(&node_path, handle.close_commit);
        let out = FuseWriteOut {
            size: count,
            padding: 0,
        };
        reply_struct(file, header.unique, &out)?;
        if invalidate_after_reply {
            self.invalidate_namespace_bindings_after_reply(file, "namespace-changing write");
        }
        Ok(())
    }

    pub(in crate::fuse) fn release(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
        payload: &[u8],
    ) -> Result<()> {
        let input = read_struct::<FuseReleaseIn>(payload)?;
        let node_path = {
            let nodes = self.nodes()?;
            nodes
                .node(header.nodeid)
                .map(|node| node.path.clone())
                .unwrap_or_default()
        };
        let handle = self.nodes()?.remove_handle(input.fh);
        let mut invalidate_after_reply = false;
        if let Some(handle) = handle {
            if handle.close_commit_flushed {
                return reply_empty(file, header.unique);
            }
            invalidate_after_reply = handle.write_on_release
                && handle.close_commit
                && refreshes_namespace_bindings(&node_path);
            if let Err(error) = handle
                .client
                .clunk_timeout(handle.fid, self.control_timeout())
            {
                if !handle.write_on_release {
                    return reply_empty(file, header.unique);
                }
                if is_transport_error(&error) {
                    self.reconnect()?;
                    return Err(Error::new(
                        libc::EIO,
                        "release failed during reconnect; close result is unknown",
                    ));
                }
                if is_namespace_shape_error(&error) {
                    self.refresh_node(header.nodeid)?;
                    return Err(Error::new(
                        libc::EIO,
                        "release failed during namespace refresh; close result is unknown",
                    ));
                }
                return Err(error);
            }
        }
        reply_empty(file, header.unique)?;
        if invalidate_after_reply {
            self.invalidate_namespace_bindings_after_reply(file, "close-commit release");
        }
        Ok(())
    }
}

fn symlink_read_count(stat: &r9p::stat::Stat) -> Result<u32> {
    let count = stat.length.clamp(1, 1024 * 1024);
    u32::try_from(count).map_err(|_| Error::new(libc::EINVAL, "symlink target too large"))
}

fn read_is_known_eof(known_length: u64, offset: u64) -> bool {
    known_length > 0 && offset >= known_length
}

#[cfg(test)]
mod tests {
    use super::read_is_known_eof;

    #[test]
    fn read_at_known_positive_length_is_eof() {
        assert!(read_is_known_eof(26_698, 26_698));
        assert!(read_is_known_eof(26_698, 30_000));
    }

    #[test]
    fn unknown_zero_length_does_not_short_circuit_dynamic_reads() {
        assert!(!read_is_known_eof(0, 0));
        assert!(!read_is_known_eof(0, 32));
    }

    #[test]
    fn read_before_known_length_reaches_9p() {
        assert!(!read_is_known_eof(26_698, 26_697));
    }
}
