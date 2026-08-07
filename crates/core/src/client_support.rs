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

/// A short Rwalk carries no reason, so "partial walk" alone cannot say whether
/// the element is absent or withheld. 9P2000 does require Rerror when the
/// *first* element fails, so walking the stopping element on its own from the
/// last good one asks the same question in a form that must be answered.
pub(crate) fn partial_walk(names: &[Vec<u8>], walked: usize, reason: Option<Error>) -> Error {
    let stopped = names
        .get(walked)
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .unwrap_or_else(|| "?".to_string());
    let position = walked.saturating_add(1);
    let total = names.len();
    match reason {
        Some(reason) => Error::from(format!(
            "partial walk: stopped at {stopped:?} ({position} of {total}): {reason}"
        )),
        None => Error::from(format!(
            "partial walk: stopped at {stopped:?} ({position} of {total}), and walking it alone did not say why"
        )),
    }
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

pub(crate) fn read_delimited_with<F>(
    mut offset: u64,
    count: u32,
    delimiter: u8,
    mut read_once: F,
) -> Result<Vec<u8>>
where
    F: FnMut(u64, u32) -> Result<Vec<u8>>,
{
    if count == 0 {
        return Err(Error::from(
            "delimiter-terminated 9P read requires a nonzero byte bound",
        ));
    }

    let mut remaining = count;
    let mut out = Vec::with_capacity(usize::try_from(count).unwrap_or(0));
    while remaining > 0 {
        let data = checked_read_data(read_once(offset, remaining)?, remaining)?;
        if data.is_empty() {
            return Err(Error::from(
                "9P read reached EOF before the record delimiter",
            ));
        }
        if let Some(position) = data.iter().position(|byte| *byte == delimiter) {
            if position + 1 != data.len() {
                return Err(Error::from(
                    "9P read returned bytes after the record delimiter",
                ));
            }
            out.extend(data);
            return Ok(out);
        }

        let read_count =
            u32::try_from(data.len()).map_err(|_| Error::from("read count overflow"))?;
        out.extend(data);
        offset = checked_advance_offset(offset, u64::from(read_count))?;
        remaining = remaining.saturating_sub(read_count);
    }

    Err(Error::from(
        "9P read reached its byte bound before the record delimiter",
    ))
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
    use super::{
        checked_advance_offset, checked_read_data, checked_write_count, read_delimited_with,
    };

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

    #[test]
    fn delimited_read_stops_without_an_eof_probe() {
        let mut reads = Vec::new();
        let bytes = read_delimited_with(0, 64, b'\n', |offset, count| {
            reads.push((offset, count));
            Ok(b"{\"ready\":true}\n".to_vec())
        })
        .expect("delimited document");

        assert_eq!(bytes, b"{\"ready\":true}\n");
        assert_eq!(reads, vec![(0, 64)]);
    }

    #[test]
    fn delimited_read_continues_across_bounded_chunks() {
        let mut reads = 0;
        let bytes = read_delimited_with(4, 16, b'\n', |offset, _| {
            reads += 1;
            match offset {
                4 => Ok(b"first ".to_vec()),
                10 => Ok(b"record\n".to_vec()),
                _ => Ok(Vec::new()),
            }
        })
        .expect("chunked delimited document");

        assert_eq!(bytes, b"first record\n");
        assert_eq!(reads, 2);
    }

    #[test]
    fn delimited_read_rejects_ambiguous_record_framing() {
        let trailing = read_delimited_with(0, 32, b'\n', |_, _| Ok(b"first\nsecond\n".to_vec()))
            .expect_err("trailing record must fail");
        assert!(trailing
            .display_lossy()
            .contains("bytes after the record delimiter"));

        let missing = read_delimited_with(0, 5, b'\n', |_, _| Ok(b"12345".to_vec()))
            .expect_err("missing delimiter must fail");
        assert!(missing
            .display_lossy()
            .contains("byte bound before the record delimiter"));
    }
}
