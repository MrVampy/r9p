//! `open` / `read` / `write` / `release` op handlers.

use super::namespace_change::{
    close_commit_refreshes_namespace_bindings, write_refreshes_namespace_bindings,
    write_uses_control_timeout,
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
            Err(error)
                if is_transport_error(&error)
                    && read_handle_is_replayable(handle.is_dir, handle.write_on_release) =>
            {
                self.reconnect()?;
                self.read_from_reopened_handle(header.nodeid, input.fh, input.offset, input.size)?
            }
            Err(error) if is_transport_error(&error) => {
                self.reconnect()?;
                return Err(Error::new(libc::ESTALE, "file handle is not replayable"));
            }
            Err(error)
                if is_namespace_shape_error(&error)
                    && read_handle_is_replayable(handle.is_dir, handle.write_on_release) =>
            {
                self.refresh_node(header.nodeid)?;
                self.read_from_reopened_handle(header.nodeid, input.fh, input.offset, input.size)?
            }
            Err(error) if is_namespace_shape_error(&error) => {
                self.refresh_node(header.nodeid)?;
                return Err(Error::new(libc::ESTALE, "file handle is not replayable"));
            }
            Err(error) => return Err(error),
        };
        reply_bytes(file, header.unique, &data)
    }

    fn read_from_reopened_handle(
        &mut self,
        nodeid: u64,
        handle_id: u64,
        offset: u64,
        size: u32,
    ) -> Result<Vec<u8>> {
        let (client, node_fid) = self.bound_node_fid(nodeid)?;
        let fid = client.clone_fid_timeout(node_fid, self.lookup_timeout())?;
        if let Err(error) = client.open_timeout(fid, OREAD, self.lookup_timeout()) {
            let _ = client.clunk_timeout(fid, self.control_timeout());
            return Err(error);
        }
        let old_handle =
            match self
                .nodes()?
                .replace_read_handle_binding(handle_id, client.clone(), fid)
            {
                Ok(old_handle) => old_handle,
                Err(error) => {
                    let _ = client.clunk_timeout(fid, self.control_timeout());
                    return Err(error);
                }
            };
        let _ = old_handle
            .client
            .clunk_timeout(old_handle.fid, self.control_timeout());
        client.read_timeout(fid, offset, size, self.read_timeout())
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
        let write_timeout = if write_uses_control_timeout(&node_path, handle.close_commit) {
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
        let handle = self.nodes()?.remove_handle(input.fh);
        let mut invalidate_after_reply = false;
        if let Some(handle) = handle {
            if handle.close_commit_flushed {
                return reply_empty(file, header.unique);
            }
            invalidate_after_reply = handle.write_on_release
                && handle.close_commit
                && close_commit_refreshes_namespace_bindings(handle.close_commit);
            match handle
                .client
                .clunk_timeout(handle.fid, self.control_timeout())
            {
                Ok(()) => {}
                Err(_) if !handle.write_on_release => {
                    return reply_empty(file, header.unique);
                }
                Err(error) if is_transport_error(&error) => {
                    self.reconnect()?;
                    return Err(Error::new(
                        libc::EIO,
                        "release failed during reconnect; close result is unknown",
                    ));
                }
                Err(error) if is_namespace_shape_error(&error) => {
                    if invalidate_after_reply {
                        self.record_diagnostic_with_context(
                            "close_commit_namespace_shape_acknowledged",
                            header,
                            0,
                            "release saw namespace refresh after close-commit; acknowledging close and invalidating bindings",
                            crate::diagnostics::DiagnosticContext {
                                fh: Some(input.fh),
                                ..self.diagnostic_context(header, payload)
                            },
                        );
                    } else {
                        self.refresh_node(header.nodeid)?;
                        return Err(Error::new(
                            libc::EIO,
                            "release failed during namespace refresh; close result is unknown",
                        ));
                    }
                }
                Err(error) => return Err(error),
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

fn read_handle_is_replayable(is_dir: bool, write_on_release: bool) -> bool {
    !is_dir && !write_on_release
}

#[cfg(test)]
mod tests {
    use super::{read_handle_is_replayable, read_is_known_eof};

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

    #[test]
    fn only_read_only_file_handles_are_replayable() {
        assert!(read_handle_is_replayable(false, false));
        assert!(!read_handle_is_replayable(false, true));
        assert!(!read_handle_is_replayable(true, false));
    }
}
