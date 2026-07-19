use crate::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestLimits {
    max_request_bytes: usize,
    max_connection_bytes: usize,
}

impl RequestLimits {
    pub fn new(max_request_bytes: usize, max_connection_bytes: usize) -> Result<Self> {
        if max_request_bytes == 0
            || max_connection_bytes == 0
            || max_request_bytes > max_connection_bytes
        {
            return Err(Error::from_static("invalid RPC request limits"));
        }
        Ok(Self {
            max_request_bytes,
            max_connection_bytes,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestChunk {
    starts_request: bool,
    resulting_request_bytes: usize,
    count: u32,
}

impl RequestChunk {
    pub fn starts_request(self) -> bool {
        self.starts_request
    }

    pub fn resulting_request_bytes(self) -> usize {
        self.resulting_request_bytes
    }

    pub fn count(self) -> u32 {
        self.count
    }
}

/// Validates one chunk in a write-then-read, same-fid RPC request.
///
/// The caller owns the request bytes and decides whether they need a sensitive
/// buffer. Pass the current request length for this fid, the bytes buffered for
/// other fids on the same connection, and the incoming `Twrite` fields. Offset
/// zero starts or replaces a request; later chunks must be exactly contiguous.
pub fn validate_request_chunk(
    current_request_bytes: Option<usize>,
    buffered_elsewhere: usize,
    offset: u64,
    chunk_bytes: usize,
    limits: RequestLimits,
) -> Result<RequestChunk> {
    let offset = usize::try_from(offset).map_err(|_| Error::from_static("RPC offset too large"))?;
    let starts_request = offset == 0;
    let base = if starts_request {
        0
    } else {
        let current = current_request_bytes
            .ok_or_else(|| Error::from_static("RPC request must begin at offset zero"))?;
        if offset != current {
            return Err(Error::from_static("RPC write offset is not contiguous"));
        }
        current
    };
    let resulting_request_bytes = base
        .checked_add(chunk_bytes)
        .ok_or_else(|| Error::from_static("RPC request size overflow"))?;
    if resulting_request_bytes > limits.max_request_bytes {
        return Err(Error::from_static("RPC request too large"));
    }
    let connection_bytes = buffered_elsewhere
        .checked_add(resulting_request_bytes)
        .ok_or_else(|| Error::from_static("RPC connection buffer size overflow"))?;
    if connection_bytes > limits.max_connection_bytes {
        return Err(Error::from_static("RPC connection buffer limit exceeded"));
    }
    let count = u32::try_from(chunk_bytes)
        .map_err(|_| Error::from_static("RPC request chunk too large"))?;
    Ok(RequestChunk {
        starts_request,
        resulting_request_bytes,
        count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: RequestLimits = RequestLimits {
        max_request_bytes: 16,
        max_connection_bytes: 24,
    };

    #[test]
    fn accepts_a_start_and_exactly_contiguous_continuation() {
        let start = validate_request_chunk(None, 4, 0, 8, LIMITS).expect("request start");
        assert!(start.starts_request());
        assert_eq!(start.resulting_request_bytes(), 8);
        assert_eq!(start.count(), 8);

        let continuation =
            validate_request_chunk(Some(8), 4, 8, 6, LIMITS).expect("request continuation");
        assert!(!continuation.starts_request());
        assert_eq!(continuation.resulting_request_bytes(), 14);
        assert_eq!(continuation.count(), 6);
    }

    #[test]
    fn offset_zero_replaces_the_current_request() {
        let replacement =
            validate_request_chunk(Some(15), 8, 0, 4, LIMITS).expect("request replacement");
        assert!(replacement.starts_request());
        assert_eq!(replacement.resulting_request_bytes(), 4);
    }

    #[test]
    fn rejects_missing_starts_gaps_and_bounded_size_overruns() {
        assert_eq!(
            validate_request_chunk(None, 0, 2, 1, LIMITS)
                .expect_err("missing start")
                .message(),
            b"RPC request must begin at offset zero"
        );
        assert_eq!(
            validate_request_chunk(Some(3), 0, 2, 1, LIMITS)
                .expect_err("gap")
                .message(),
            b"RPC write offset is not contiguous"
        );
        assert_eq!(
            validate_request_chunk(Some(12), 0, 12, 5, LIMITS)
                .expect_err("request bound")
                .message(),
            b"RPC request too large"
        );
        assert_eq!(
            validate_request_chunk(Some(12), 9, 12, 3, LIMITS)
                .expect_err("connection bound")
                .message(),
            b"RPC connection buffer limit exceeded"
        );
    }
}
