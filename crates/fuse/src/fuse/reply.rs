//! FUSE reply framing and small byte-slice helpers shared across the
//! dispatcher and op handlers.

use super::wire::FuseOutHeader;
use crate::error::{Error, Result};
use std::{
    fs::File,
    io::{IoSlice, Write},
    mem::size_of,
    ptr,
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
    // rejected. writev hands header and body to the kernel atomically, so no
    // intermediate buffer is needed.
    let slices = [IoSlice::new(as_bytes(&header)), IoSlice::new(payload)];
    let written = file
        .write_vectored(&slices)
        .map_err(|error| Error::io("write FUSE reply", error))?;
    if written != len {
        return Err(Error::new(libc::EIO, "short FUSE reply write"));
    }
    Ok(())
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
