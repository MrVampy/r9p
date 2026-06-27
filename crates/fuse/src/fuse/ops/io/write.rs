use super::super::namespace_change::{
    close_commit_refreshes_namespace_bindings, write_can_replay_after_namespace_refresh,
    write_refreshes_namespace_bindings, write_uses_control_timeout,
};
use crate::{
    error::{Error, Result},
    fuse::{
        reply::{read_struct, reply_empty, reply_struct},
        util::{is_namespace_shape_error, is_transport_error},
        wire::{FuseInHeader, FuseReleaseIn, FuseWriteIn, FuseWriteOut},
        R9pFuse,
    },
    p9::OWRITE,
};
use std::{fs::File, mem::size_of};

impl R9pFuse {
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
            Err(error)
                if is_namespace_shape_error(&error)
                    && write_can_replay_after_namespace_refresh(
                        &node_path,
                        handle.close_commit,
                        input.offset,
                    ) =>
            {
                self.record_diagnostic_with_context(
                    "control_write_replay_after_namespace_refresh",
                    header,
                    error.errno,
                    error.message().to_string(),
                    self.diagnostic_context(header, payload),
                );
                self.recover_namespace_shape(header.nodeid)?;
                self.write_from_reopened_write_handle(
                    header.nodeid,
                    input.fh,
                    input.offset,
                    data,
                    write_timeout,
                )?
            }
            Err(error) if is_namespace_shape_error(&error) => {
                self.recover_namespace_shape(header.nodeid)?;
                return Err(Error::new(
                    libc::EIO,
                    "write failed during namespace refresh and was not replayed",
                ));
            }
            Err(error) => return Err(error),
        };
        if count > 0 {
            self.nodes()?.note_handle_write(input.fh, count)?;
        }
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

    fn write_from_reopened_write_handle(
        &mut self,
        nodeid: u64,
        handle_id: u64,
        offset: u64,
        data: &[u8],
        write_timeout: std::time::Duration,
    ) -> Result<u32> {
        let (client, node_fid) = self.bound_node_fid(nodeid)?;
        let fid = client.clone_fid_timeout(node_fid, self.control_timeout())?;
        if let Err(error) = client.open_timeout(fid, OWRITE, self.control_timeout()) {
            let _ = client.clunk_timeout(fid, self.control_timeout());
            return Err(error);
        }
        let old_handle =
            match self
                .nodes()?
                .replace_write_handle_binding(handle_id, client.clone(), fid)
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
        client.write_timeout(fid, offset, data, write_timeout)
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
                        self.recover_namespace_shape(header.nodeid)?;
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
