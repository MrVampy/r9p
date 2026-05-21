//! Stateless conversion helpers between the FUSE and 9P views of the world.

use super::wire::{FuseDirent, FOPEN_DIRECT_IO};
use crate::{
    error::Error,
    p9::{ORDWR, OREAD, OTRUNC, OWRITE},
};
use std::{mem::size_of, time::Duration};

pub(super) fn flags_to_9p_mode(flags: u32) -> u8 {
    let mut mode = match flags & libc::O_ACCMODE as u32 {
        x if x == libc::O_WRONLY as u32 => OWRITE,
        x if x == libc::O_RDWR as u32 => ORDWR,
        _ => OREAD,
    };
    if flags & libc::O_TRUNC as u32 != 0 {
        mode |= OTRUNC;
    }
    mode
}

pub(super) fn fuse_open_flags(is_dir_open: bool, mode: u8) -> u32 {
    if is_dir_open || mode != OREAD {
        0
    } else {
        FOPEN_DIRECT_IO
    }
}

pub(super) fn is_transport_error(error: &Error) -> bool {
    matches!(
        error.errno,
        libc::EPIPE
            | libc::ECONNRESET
            | libc::ECONNABORTED
            | libc::ENOTCONN
            | libc::ESHUTDOWN
            | libc::ETIMEDOUT
            | libc::EAGAIN
    ) || (error.errno == libc::EIO && is_transport_message(error.message()))
}

fn is_transport_message(message: &str) -> bool {
    message.contains("9P frame")
        || message.contains("9P reader stopped")
        || message.contains("9P response timeout")
        || message.contains("clone 9P stream")
}

/// Return true for errors that can mean "the server-side namespace/session shape
/// no longer matches the path-backed nodes this mount has already walked".
///
/// Callers must still decide whether the operation is safe to replay. This is
/// intentionally a *candidate* predicate: a genuine missing path is also
/// `ENOENT`, and an `unknown fid` can mean an open handle is no longer safe, so
/// callers retry at most once and only at a safe operation boundary.
pub(super) fn is_namespace_shape_error(error: &Error) -> bool {
    error.errno == libc::ENOENT
        || (error.errno == libc::ESTALE && is_stale_namespace_message(error.message()))
}

fn is_stale_namespace_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("unknown fid") || lower.contains("stale fid")
}

pub(super) fn duration_parts(duration: Duration) -> (u64, u32) {
    (duration.as_secs(), duration.subsec_nanos())
}

pub(super) fn dirent_size(name_len: usize) -> usize {
    (size_of::<FuseDirent>() - 1 + name_len + 7) & !7
}
