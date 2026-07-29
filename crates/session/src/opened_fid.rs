use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
    time::Duration,
};

use r9p::{multiplex::DelimitedRead, Fid, Tag, OREAD};

use crate::{Client, Error, Result, WriteThenReadError};

/// An opened 9P fid retained for repeated, ordered operations.
///
/// Keeping the fid alive avoids repeating walk/open/clunk for a hot namespace
/// file. Methods require mutable access so callers cannot accidentally issue
/// overlapping operations whose file-level ordering would be ambiguous.
pub struct OpenedFid {
    client: Client,
    fid: Option<Fid>,
    clunk_timeout: Duration,
}

/// One read-only 9P fid shared by concurrent blocking reads.
///
/// This is an explicit opt-in for files whose read offsets identify
/// independent, replay-safe application observations. Clones share the same
/// opened fid. [`Self::cancel`] flushes every outstanding request tag before
/// clunking the fid.
#[derive(Clone)]
pub struct ConcurrentReadFid {
    inner: Arc<ConcurrentReadFidInner>,
}

struct ConcurrentReadFidInner {
    client: Client,
    state: Mutex<ConcurrentReadFidState>,
    clunk_timeout: Duration,
}

struct ConcurrentReadFidState {
    fid: Option<Fid>,
    active: BTreeSet<Tag>,
}

struct ActiveConcurrentRead {
    inner: Arc<ConcurrentReadFidInner>,
    tag: Tag,
}

impl Client {
    pub fn open_path_timeout(&self, path: &str, mode: u8, timeout: Duration) -> Result<OpenedFid> {
        let fid = open_path_fid_timeout(self, path, mode, timeout)?;
        Ok(OpenedFid {
            client: self.clone(),
            fid: Some(fid),
            clunk_timeout: timeout,
        })
    }

    pub fn open_concurrent_read_path_timeout(
        &self,
        path: &str,
        timeout: Duration,
    ) -> Result<ConcurrentReadFid> {
        let fid = open_path_fid_timeout(self, path, OREAD, timeout)?;
        Ok(ConcurrentReadFid {
            inner: Arc::new(ConcurrentReadFidInner {
                client: self.clone(),
                state: Mutex::new(ConcurrentReadFidState {
                    fid: Some(fid),
                    active: BTreeSet::new(),
                }),
                clunk_timeout: timeout,
            }),
        })
    }
}

impl OpenedFid {
    pub fn read_timeout(&mut self, offset: u64, count: u32, timeout: Duration) -> Result<Vec<u8>> {
        self.client
            .read_timeout(self.required_fid()?, offset, count, timeout)
    }

    pub fn read_full_timeout(
        &mut self,
        offset: u64,
        count: u32,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        self.client
            .read_full_timeout(self.required_fid()?, offset, count, timeout)
    }

    pub fn read_delimited_timeout(
        &mut self,
        offset: u64,
        count: u32,
        delimiter: u8,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        self.client
            .read_delimited_timeout(self.required_fid()?, offset, count, delimiter, timeout)
    }

    pub fn read_full(&mut self, offset: u64, count: u32) -> Result<Vec<u8>> {
        self.client.read_full(self.required_fid()?, offset, count)
    }

    pub fn read_delimited(&mut self, offset: u64, count: u32, delimiter: u8) -> Result<Vec<u8>> {
        self.client
            .read_delimited(self.required_fid()?, offset, count, delimiter)
    }

    pub fn write_timeout(&mut self, offset: u64, data: &[u8], timeout: Duration) -> Result<u32> {
        self.client
            .write_timeout(self.required_fid()?, offset, data, timeout)
    }

    /// Pipelines the final request write with the first response read on this
    /// retained fid and returns one bounded delimiter-terminated record.
    pub fn write_then_read_delimited_timeout(
        &mut self,
        write_offset: u64,
        data: &[u8],
        read: DelimitedRead,
        timeout: Duration,
    ) -> std::result::Result<(u32, Vec<u8>), WriteThenReadError> {
        let fid = self.required_fid().map_err(WriteThenReadError::Rejected)?;
        self.client
            .write_then_read_delimited_timeout(fid, write_offset, data, read, timeout)
    }

    pub fn close(mut self) -> Result<()> {
        self.clunk()
    }

    fn required_fid(&self) -> Result<Fid> {
        self.fid
            .ok_or_else(|| Error::new(libc::EBADF, "opened fid is closed"))
    }

    fn clunk(&mut self) -> Result<()> {
        let Some(fid) = self.fid.take() else {
            return Ok(());
        };
        self.client.clunk_timeout(fid, self.clunk_timeout)
    }
}

impl Drop for OpenedFid {
    fn drop(&mut self) {
        let _ = self.clunk();
    }
}

impl ConcurrentReadFid {
    /// Blocks until this positional read completes, the transport fails, or
    /// the request is explicitly cancelled.
    pub fn read(&self, offset: u64, count: u32) -> Result<Vec<u8>> {
        let pending = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| Error::new(libc::EIO, "concurrent read fid lock poisoned"))?;
            let fid = state
                .fid
                .ok_or_else(|| Error::new(libc::EBADF, "concurrent read fid is closed"))?;
            let pending = self.inner.client.submit_read(fid, offset, count)?;
            state.active.insert(pending.tag());
            pending
        };
        let tag = pending.tag();
        let _active = ActiveConcurrentRead {
            inner: Arc::clone(&self.inner),
            tag,
        };
        pending.wait()
    }

    pub fn read_timeout(&self, offset: u64, count: u32, timeout: Duration) -> Result<Vec<u8>> {
        let pending = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| Error::new(libc::EIO, "concurrent read fid lock poisoned"))?;
            let fid = state
                .fid
                .ok_or_else(|| Error::new(libc::EBADF, "concurrent read fid is closed"))?;
            let pending = self.inner.client.submit_read(fid, offset, count)?;
            state.active.insert(pending.tag());
            pending
        };
        let tag = pending.tag();
        let _active = ActiveConcurrentRead {
            inner: Arc::clone(&self.inner),
            tag,
        };
        pending.wait_timeout(timeout)
    }

    /// Reads one delimiter-terminated positional record without turning an
    /// idle blocking subscription into periodic cancellation and resubmission.
    pub fn read_delimited(&self, offset: u64, count: u32, delimiter: u8) -> Result<Vec<u8>> {
        read_delimited_with(offset, count, delimiter, |offset, remaining| {
            self.read(offset, remaining)
        })
    }

    pub fn read_delimited_timeout(
        &self,
        offset: u64,
        count: u32,
        delimiter: u8,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        read_delimited_with(offset, count, delimiter, |offset, remaining| {
            self.read_timeout(offset, remaining, timeout)
        })
    }

    /// Flushes every outstanding read tag, then clunks the shared fid.
    ///
    /// Every clone becomes closed. Repeated cancellation is idempotent.
    pub fn cancel(&self) -> Result<()> {
        self.inner.clunk()
    }

    pub fn close(self) -> Result<()> {
        self.cancel()
    }
}

impl ConcurrentReadFidInner {
    fn clunk(&self) -> Result<()> {
        let (fid, active) = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| Error::new(libc::EIO, "concurrent read fid lock poisoned"))?;
            let fid = state.fid.take();
            let active = state.active.iter().copied().collect::<Vec<_>>();
            (fid, active)
        };
        let Some(fid) = fid else {
            return Ok(());
        };
        for tag in active {
            let _ = self.client.flush_read_tag_timeout(
                fid,
                tag,
                self.clunk_timeout.min(Duration::from_millis(250)),
            );
        }
        self.client.clunk_timeout(fid, self.clunk_timeout)
    }

    fn finish(&self, tag: Tag) {
        if let Ok(mut state) = self.state.lock() {
            state.active.remove(&tag);
        }
    }
}

impl Drop for ActiveConcurrentRead {
    fn drop(&mut self) {
        self.inner.finish(self.tag);
    }
}

impl Drop for ConcurrentReadFidInner {
    fn drop(&mut self) {
        let _ = self.clunk();
    }
}

fn open_path_fid_timeout(client: &Client, path: &str, mode: u8, timeout: Duration) -> Result<Fid> {
    let names = path_names(path)?;
    let fid = if names.is_empty() {
        client.clone_fid_timeout(client.root_fid(), timeout)?
    } else {
        client.walk_timeout(client.root_fid(), &names, timeout)?
    };
    if let Err(error) = client.open_timeout(fid, mode, timeout) {
        let _ = client.clunk_timeout(fid, timeout);
        return Err(error);
    }
    Ok(fid)
}

fn path_names(path: &str) -> Result<Vec<Vec<u8>>> {
    if path.is_empty() || path == "/" {
        return Ok(Vec::new());
    }
    let path = path.strip_prefix('/').unwrap_or(path);
    if path.is_empty() || path.ends_with('/') {
        return Err(Error::new(libc::EINVAL, "invalid 9P path"));
    }
    path.split('/')
        .map(|name| {
            if name.is_empty() {
                Err(Error::new(libc::EINVAL, "invalid 9P path"))
            } else {
                Ok(name.as_bytes().to_vec())
            }
        })
        .collect()
}

fn read_delimited_with<F>(
    mut offset: u64,
    count: u32,
    delimiter: u8,
    mut read_once: F,
) -> Result<Vec<u8>>
where
    F: FnMut(u64, u32) -> Result<Vec<u8>>,
{
    if count == 0 {
        return Err(Error::new(
            libc::EINVAL,
            "delimiter-terminated 9P read requires a nonzero byte bound",
        ));
    }
    let mut remaining = count;
    let mut out = Vec::with_capacity(usize::try_from(count).unwrap_or(0));
    while remaining > 0 {
        let data = read_once(offset, remaining)?;
        if data.is_empty() {
            return Err(Error::new(
                libc::EIO,
                "9P read reached EOF before the record delimiter",
            ));
        }
        if let Some(position) = data.iter().position(|byte| *byte == delimiter) {
            if position + 1 != data.len() {
                return Err(Error::new(
                    libc::EPROTO,
                    "9P read returned bytes after the record delimiter",
                ));
            }
            out.extend(data);
            return Ok(out);
        }
        let read_count = u32::try_from(data.len())
            .map_err(|_| Error::new(libc::EOVERFLOW, "read count overflow"))?;
        out.extend(data);
        offset = offset
            .checked_add(u64::from(read_count))
            .ok_or_else(|| Error::new(libc::EOVERFLOW, "9P offset overflow"))?;
        remaining = remaining.saturating_sub(read_count);
    }
    Err(Error::new(
        libc::EMSGSIZE,
        "9P read reached its byte bound before the record delimiter",
    ))
}

#[cfg(test)]
mod tests {
    use super::path_names;

    #[test]
    fn path_names_accept_root_absolute_and_relative_paths() {
        assert_eq!(path_names("/").expect("root"), Vec::<Vec<u8>>::new());
        assert_eq!(
            path_names("/agents/status").expect("absolute"),
            vec![b"agents".to_vec(), b"status".to_vec()]
        );
        assert_eq!(
            path_names("agents/status").expect("relative"),
            vec![b"agents".to_vec(), b"status".to_vec()]
        );
    }

    #[test]
    fn path_names_reject_empty_segments() {
        assert!(path_names("/agents//status").is_err());
        assert!(path_names("/agents/status/").is_err());
    }
}
