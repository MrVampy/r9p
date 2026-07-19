use crate::{
    client::Op,
    error::{Error, Result},
    fid::Fid,
};
use std::io;

pub(crate) fn op_fid(op: &Op) -> Result<Fid> {
    op.fid
        .ok_or_else(|| Error::from("9P operation did not allocate a fid"))
}

pub(crate) fn protocol_error(error: Error) -> Error {
    Error::from(format!("9P client state: {error}"))
}

pub(crate) fn io_error(context: impl AsRef<str>, error: io::Error) -> Error {
    if matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ) {
        return Error::from(format!(
            "{}: 9P transport timeout or would-block: {error}",
            context.as_ref()
        ));
    }
    Error::from(format!("{}: {error}", context.as_ref()))
}

pub(crate) fn unexpected(expected: &str, got: impl std::fmt::Debug) -> Error {
    Error::from(format!("expected {expected}, got {got:?}"))
}

pub(crate) fn write_in_chunks<F>(
    max_write_payload: u32,
    mut offset: u64,
    mut data: &[u8],
    mut write_once: F,
) -> Result<u32>
where
    F: FnMut(u64, &[u8]) -> Result<u32>,
{
    if data.is_empty() {
        return write_once(offset, data);
    }

    let mut total = 0_u32;
    let max = usize::try_from(max_write_payload).unwrap_or(usize::MAX);
    while !data.is_empty() {
        let chunk_len = data.len().min(max);
        let chunk = &data[..chunk_len];
        let count = write_once(offset, chunk)?;
        if count == 0 {
            return Err(Error::from("zero-length 9P write progress"));
        }
        let count_usize =
            usize::try_from(count).map_err(|_| Error::from("write count overflow"))?;
        if count_usize > chunk_len {
            return Err(Error::from(
                "9P server reported more bytes written than requested",
            ));
        }
        total = total.saturating_add(count);
        offset = offset.saturating_add(u64::from(count));
        data = &data[count_usize..];
        if count_usize < chunk_len {
            break;
        }
    }
    Ok(total)
}
