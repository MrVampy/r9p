//! FUSE reply framing and small byte-slice helpers shared across the
//! dispatcher and op handlers.

use super::wire::{
    FuseNotifyInvalEntryOut, FuseNotifyInvalInodeOut, FuseOutHeader, FUSE_NOTIFY_INVAL_ENTRY,
    FUSE_NOTIFY_INVAL_INODE,
};
use crate::error::{Error, Result};
use std::{
    fs::File,
    io::{IoSlice, Write},
    mem::size_of,
    ptr,
    sync::{Mutex, OnceLock},
};

pub(super) fn reply_empty(file: &mut File, unique: u64) -> Result<()> {
    reply_bytes(file, unique, &[])
}

pub(super) fn reply_error(file: &mut File, unique: u64, errno: i32) -> Result<()> {
    let header = FuseOutHeader {
        len: u32::try_from(size_of::<FuseOutHeader>()).unwrap_or(u32::MAX),
        error: -errno.abs(),
        unique,
    };
    let _guard = reply_write_guard()?;
    file.write_all(as_bytes(&header))
        .map_err(|error| Error::io("write FUSE error reply", error))
}

pub(super) fn reply_struct<T>(file: &mut File, unique: u64, payload: &T) -> Result<()> {
    reply_bytes(file, unique, as_bytes(payload))
}

pub(super) fn reply_bytes(file: &mut File, unique: u64, payload: &[u8]) -> Result<()> {
    let len = size_of::<FuseOutHeader>()
        .checked_add(payload.len())
        .ok_or_else(|| Error::new(libc::EOVERFLOW, "FUSE reply too large"))?;
    let header = FuseOutHeader {
        len: u32::try_from(len).map_err(|_| Error::new(libc::EOVERFLOW, "FUSE reply too large"))?,
        error: 0,
        unique,
    };
    // /dev/fuse consumes each reply as one message; a partial write would be
    // rejected. writev hands header and body to the kernel in one syscall, and
    // the process-wide guard keeps concurrent worker replies from interleaving
    // on cloned /dev/fuse descriptors.
    let slices = [IoSlice::new(as_bytes(&header)), IoSlice::new(payload)];
    let _guard = reply_write_guard()?;
    let written = file
        .write_vectored(&slices)
        .map_err(|error| Error::io("write FUSE reply", error))?;
    if written != len {
        return Err(Error::new(libc::EIO, "short FUSE reply write"));
    }
    Ok(())
}

pub(super) fn notify_inval_inode(file: &mut File, nodeid: u64) -> Result<()> {
    let payload = FuseNotifyInvalInodeOut {
        ino: nodeid,
        off: 0,
        len: 0,
    };
    notify_bytes(
        file,
        FUSE_NOTIFY_INVAL_INODE,
        &[IoSlice::new(as_bytes(&payload))],
    )
}

pub(super) fn notify_inval_entry(file: &mut File, parent: u64, name: &[u8]) -> Result<()> {
    let namelen = u32::try_from(name.len())
        .map_err(|_| Error::new(libc::ENAMETOOLONG, "FUSE entry name too long"))?;
    let payload = FuseNotifyInvalEntryOut {
        parent,
        namelen,
        flags: 0,
    };
    let nul = [0_u8; 1];
    notify_bytes(
        file,
        FUSE_NOTIFY_INVAL_ENTRY,
        &[
            IoSlice::new(as_bytes(&payload)),
            IoSlice::new(name),
            IoSlice::new(&nul),
        ],
    )
}

fn notify_bytes(file: &mut File, code: i32, payloads: &[IoSlice<'_>]) -> Result<()> {
    let payload_len = payloads.iter().map(|slice| slice.len()).sum::<usize>();
    let len = size_of::<FuseOutHeader>()
        .checked_add(payload_len)
        .ok_or_else(|| Error::new(libc::EOVERFLOW, "FUSE notification too large"))?;
    let header = FuseOutHeader {
        len: u32::try_from(len)
            .map_err(|_| Error::new(libc::EOVERFLOW, "FUSE notification too large"))?,
        error: code,
        unique: 0,
    };
    let mut slices = Vec::with_capacity(payloads.len() + 1);
    slices.push(IoSlice::new(as_bytes(&header)));
    slices.extend_from_slice(payloads);
    let _guard = reply_write_guard()?;
    let written = file
        .write_vectored(&slices)
        .map_err(|error| Error::io("write FUSE notification", error))?;
    if written != len {
        return Err(Error::new(libc::EIO, "short FUSE notification write"));
    }
    Ok(())
}

fn reply_write_guard() -> Result<std::sync::MutexGuard<'static, ()>> {
    static REPLY_WRITE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    REPLY_WRITE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| Error::new(libc::EIO, "FUSE reply write lock poisoned"))
}

pub(super) fn as_bytes<T>(value: &T) -> &[u8] {
    unsafe { std::slice::from_raw_parts((value as *const T).cast::<u8>(), size_of::<T>()) }
}

pub(super) fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend(value.to_ne_bytes());
}

pub(super) fn push_u64(out: &mut Vec<u8>, value: u64) {
    out.extend(value.to_ne_bytes());
}

pub(super) fn c_string(bytes: &[u8]) -> Result<&[u8]> {
    let (value, _rest) = next_c_string(bytes)?;
    Ok(value)
}

pub(super) fn next_c_string(bytes: &[u8]) -> Result<(&[u8], &[u8])> {
    let nul = bytes
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(|| Error::new(libc::EINVAL, "unterminated FUSE string"))?;
    Ok((&bytes[..nul], &bytes[nul + 1..]))
}

pub(super) fn read_struct<T: Copy>(bytes: &[u8]) -> Result<T> {
    if bytes.len() < size_of::<T>() {
        return Err(Error::new(libc::EINVAL, "short FUSE payload"));
    }
    Ok(unsafe { ptr::read_unaligned(bytes.as_ptr().cast::<T>()) })
}
