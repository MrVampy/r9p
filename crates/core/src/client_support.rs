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

pub(crate) fn checked_read_data(data: Vec<u8>, requested: u32) -> Result<Vec<u8>> {
    let actual = u32::try_from(data.len()).map_err(|_| Error::from("read count overflow"))?;
    if actual > requested {
        return Err(Error::from(
            "9P server reported more bytes read than requested",
        ));
    }
    Ok(data)
}

pub(crate) fn checked_write_count(count: u32, requested: usize) -> Result<u32> {
    if usize::try_from(count).map_or(true, |count| count > requested) {
        return Err(Error::from(
            "9P server reported more bytes written than requested",
        ));
    }
    Ok(count)
}

pub(crate) fn checked_advance_offset(offset: u64, count: u64) -> Result<u64> {
    offset
        .checked_add(count)
        .ok_or_else(|| Error::from("9P offset overflow"))
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
        let count = checked_write_count(count, chunk_len)?;
        let count_usize =
            usize::try_from(count).map_err(|_| Error::from("write count overflow"))?;
        total = total
            .checked_add(count)
            .ok_or_else(|| Error::from("aggregate write count overflow"))?;
        offset = checked_advance_offset(offset, u64::from(count))?;
        data = &data[count_usize..];
        if count_usize < chunk_len {
            break;
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::{checked_advance_offset, checked_read_data, checked_write_count};

    #[test]
    fn response_counts_cannot_exceed_the_request() {
        assert!(checked_read_data(vec![0; 2], 1).is_err());
        assert!(checked_write_count(2, 1).is_err());
        assert_eq!(
            checked_read_data(vec![0], 1).expect("bounded read"),
            vec![0]
        );
        assert_eq!(checked_write_count(1, 1).expect("bounded write"), 1);
        assert!(checked_advance_offset(u64::MAX, 1).is_err());
    }
}
