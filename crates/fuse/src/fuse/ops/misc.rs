//! `statfs` / `poll` / advisory lifecycle op handlers.

use super::namespace_change::close_commit_refreshes_namespace_bindings;
use crate::{
    error::{Error, Result},
    fuse::{
        reply::{read_struct, reply_empty, reply_error, reply_struct},
        util::{is_namespace_shape_error, is_transport_error},
        wire::{
            FuseAccessIn, FuseFlushIn, FuseFsyncIn, FuseInHeader, FuseKstatfs, FusePollIn,
            FuseStatfsOut,
        },
        R9pFuse,
    },
};
use std::fs::File;

impl R9pFuse {
    pub(in crate::fuse) fn statfs(&mut self, file: &mut File, header: FuseInHeader) -> Result<()> {
        let out = FuseStatfsOut {
            st: FuseKstatfs {
                blocks: 0,
                bfree: 0,
                bavail: 0,
                files: 0,
                ffree: 0,
                bsize: 8192,
                namelen: 255,
                frsize: 8192,
                padding: 0,
                spare: [0; 6],
            },
        };
        reply_struct(file, header.unique, &out)
    }

    pub(in crate::fuse) fn poll(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
        payload: &[u8],
    ) -> Result<()> {
        let input = read_struct::<FusePollIn>(payload)?;
        self.record_diagnostic_with_context(
            "poll_unsupported",
            header,
            libc::EOPNOTSUPP,
            "FUSE poll readiness is not implemented; returning non-sticky EOPNOTSUPP",
            crate::diagnostics::DiagnosticContext {
                fh: Some(input.fh),
                ..self.diagnostic_context(header, payload)
            },
        );
        reply_error(file, header.unique, libc::EOPNOTSUPP)
    }

    pub(in crate::fuse) fn flush(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
        payload: &[u8],
    ) -> Result<()> {
        let input = read_struct::<FuseFlushIn>(payload)?;
        let handle = self.nodes()?.handle(input.fh)?.clone();
        let mut invalidate_after_reply = false;
        if flush_should_close_commit(
            handle.write_on_release,
            handle.close_commit,
            handle.close_commit_flushed,
            handle.bytes_written,
        ) {
            match handle
                .client
                .clunk_timeout(handle.fid, self.control_timeout())
            {
                Ok(()) => {}
                Err(error) if is_transport_error(&error) => {
                    self.reconnect()?;
                    return Err(Error::new(
                        libc::EIO,
                        "flush failed during reconnect; close result is unknown",
                    ));
                }
                Err(error) if is_namespace_shape_error(&error) => {
                    if close_commit_refreshes_namespace_bindings(handle.close_commit) {
                        self.record_diagnostic_with_context(
                            "close_commit_namespace_shape_acknowledged",
                            header,
                            0,
                            "flush saw namespace refresh after close-commit; acknowledging close and invalidating bindings",
                            crate::diagnostics::DiagnosticContext {
                                fh: Some(input.fh),
                                ..self.diagnostic_context(header, payload)
                            },
                        );
                    } else {
                        self.recover_namespace_shape(header.nodeid)?;
                        return Err(Error::new(
                            libc::EIO,
                            "flush failed during namespace refresh; close result is unknown",
                        ));
                    }
                }
                Err(error) => return Err(error),
            }
            self.nodes()?.mark_close_commit_flushed(input.fh)?;
            invalidate_after_reply = close_commit_refreshes_namespace_bindings(handle.close_commit);
        }
        // Plain 9P2000 has no close-time flush primitive. For close-commit
        // command files, the server deliberately commits on Tclunk so Linux
        // close(2) can receive command failures via FUSE FLUSH; FUSE RELEASE
        // errors are ignored by VFS.
        reply_empty(file, header.unique)?;
        if invalidate_after_reply {
            self.invalidate_namespace_bindings_after_reply(file, "close-commit flush");
        }
        Ok(())
    }

    pub(in crate::fuse) fn fsync(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
        payload: &[u8],
    ) -> Result<()> {
        let input = read_struct::<FuseFsyncIn>(payload)?;
        self.ensure_handle_known(input.fh)?;
        // There is no 9P2000 fsync verb to forward. Writable 9P services
        // expose durability through their own namespace control surfaces; the
        // FUSE bridge acknowledges fsync only after verifying the handle is
        // still live.
        reply_empty(file, header.unique)
    }

    pub(in crate::fuse) fn fsyncdir(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
        payload: &[u8],
    ) -> Result<()> {
        let input = read_struct::<FuseFsyncIn>(payload)?;
        let handle = self.nodes()?.handle(input.fh)?.clone();
        if !handle.is_dir {
            return reply_error(file, header.unique, libc::ENOTDIR);
        }
        reply_empty(file, header.unique)
    }

    pub(in crate::fuse) fn access(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
        payload: &[u8],
    ) -> Result<()> {
        let input = read_struct::<FuseAccessIn>(payload)?;
        if input.mask == libc::F_OK as u32 {
            return reply_empty(file, header.unique);
        }
        let stat = self.nodes()?.node(header.nodeid)?.stat.clone();
        let mode = self.attr(&stat).mode & 0o777;
        if requested_access_is_allowed(input.mask, mode) {
            reply_empty(file, header.unique)
        } else {
            reply_error(file, header.unique, libc::EACCES)
        }
    }

    fn ensure_handle_known(&self, fh: u64) -> Result<()> {
        let _ = self.nodes()?.handle(fh)?;
        Ok(())
    }
}

fn requested_access_is_allowed(mask: u32, mode: u32) -> bool {
    if mask & libc::R_OK as u32 != 0 && mode & 0o444 == 0 {
        return false;
    }
    if mask & libc::W_OK as u32 != 0 && mode & 0o222 == 0 {
        return false;
    }
    if mask & libc::X_OK as u32 != 0 && mode & 0o111 == 0 {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::{flush_should_close_commit, requested_access_is_allowed};

    #[test]
    fn access_masks_follow_synthesized_mode_bits() {
        assert!(requested_access_is_allowed(libc::R_OK as u32, 0o444));
        assert!(requested_access_is_allowed(libc::W_OK as u32, 0o600));
        assert!(requested_access_is_allowed(libc::X_OK as u32, 0o555));
        assert!(!requested_access_is_allowed(libc::W_OK as u32, 0o444));
        assert!(!requested_access_is_allowed(libc::X_OK as u32, 0o644));
    }

    #[test]
    fn close_commit_flush_waits_until_handle_has_written_bytes() {
        assert!(!flush_should_close_commit(true, true, false, 0));
        assert!(flush_should_close_commit(true, true, false, 1));
        assert!(!flush_should_close_commit(true, true, true, 1));
        assert!(!flush_should_close_commit(true, false, false, 1));
        assert!(!flush_should_close_commit(false, true, false, 1));
    }
}

fn flush_should_close_commit(
    write_on_release: bool,
    close_commit: bool,
    close_commit_flushed: bool,
    bytes_written: u64,
) -> bool {
    write_on_release && close_commit && !close_commit_flushed && bytes_written > 0
}
