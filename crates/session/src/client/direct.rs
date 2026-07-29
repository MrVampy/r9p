use crate::{
    error::client_error, request::RequestTracker, transport::connect_stream, ConnectionConfig,
    Error, Result, WriteThenReadError,
};
use r9p::{
    codec::Variant,
    fid::Fid,
    multiplex::{DelimitedRead, MultiplexedClient, PendingRead},
    qid::Qid,
    referral::NamespaceReferral,
    stat::Stat,
    Tag,
};
use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

const CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Clone)]
pub(super) struct DirectClient {
    inner: MultiplexedClient<crate::transport::ClientStream>,
    identity: Arc<()>,
}

impl DirectClient {
    pub fn connect_with_tracker_timeout(
        config: &ConnectionConfig,
        tracker: RequestTracker,
        timeout: Duration,
    ) -> Result<Self> {
        let started = Instant::now();
        loop {
            match Self::connect_with_tracker_once(config, tracker.clone(), timeout) {
                Ok(client) => return Ok(client),
                Err(error) if connect_should_retry(&error, timeout, started) => {
                    thread::sleep(connect_retry_sleep(timeout, started));
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn connect_with_tracker_once(
        config: &ConnectionConfig,
        tracker: RequestTracker,
        connect_timeout: Duration,
    ) -> Result<Self> {
        let stream = connect_stream(config, connect_timeout)?;
        let inner = MultiplexedClient::connect_with_variant(
            stream,
            &config.uname,
            &config.aname,
            config.msize,
            Variant::R,
        )
        .map_err(client_error)?;
        let tracked_inner = inner.clone();
        let tracked_requests = tracker.clone();
        let inner = inner.with_call_observer(move |tag| -> Box<dyn Send> {
            Box::new(tracked_requests.track_current(tag, tracked_inner.clone()))
        });
        Ok(Self {
            inner,
            identity: Arc::new(()),
        })
    }

    pub fn same_connection(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.identity, &other.identity)
    }

    pub fn root_fid(&self) -> Fid {
        self.inner.root_fid()
    }

    pub fn variant(&self) -> Variant {
        self.inner.variant()
    }

    pub fn msize(&self) -> u32 {
        self.inner.msize()
    }

    pub fn version(&self) -> Vec<u8> {
        self.inner.version()
    }

    pub fn root_qid(&self) -> Qid {
        self.inner.root_qid()
    }

    pub fn max_write_payload(&self) -> u32 {
        self.inner.max_write_payload()
    }

    pub fn clone_fid(&self, fid: Fid) -> Result<Fid> {
        self.inner.clone_fid(fid).map_err(client_error)
    }

    pub fn clone_fid_timeout(&self, fid: Fid, timeout: Duration) -> Result<Fid> {
        self.inner
            .clone_fid_timeout(fid, timeout)
            .map_err(client_error)
    }

    pub fn walk_timeout(&self, fid: Fid, names: &[Vec<u8>], timeout: Duration) -> Result<Fid> {
        self.inner
            .walk_timeout(fid, names, timeout)
            .map_err(client_error)
    }

    pub fn referrals_timeout(&self, timeout: Duration) -> Result<Vec<NamespaceReferral>> {
        self.inner
            .referrals_timeout(self.root_fid(), timeout)
            .map_err(client_error)
    }

    pub fn walk(&self, fid: Fid, names: &[Vec<u8>]) -> Result<Fid> {
        self.inner.walk(fid, names).map_err(client_error)
    }

    pub fn open(&self, fid: Fid, mode: u8) -> Result<Qid> {
        self.inner.open(fid, mode).map_err(client_error)
    }

    pub fn open_timeout(&self, fid: Fid, mode: u8, timeout: Duration) -> Result<Qid> {
        self.inner
            .open_timeout(fid, mode, timeout)
            .map_err(client_error)
    }

    pub fn create_timeout(
        &self,
        parent_fid: Fid,
        name: &[u8],
        perm: u32,
        mode: u8,
        timeout: Duration,
    ) -> Result<(Fid, Qid)> {
        self.inner
            .create_timeout(parent_fid, name, perm, mode, timeout)
            .map_err(client_error)
    }

    pub fn create(&self, parent_fid: Fid, name: &[u8], perm: u32, mode: u8) -> Result<(Fid, Qid)> {
        self.inner
            .create(parent_fid, name, perm, mode)
            .map_err(client_error)
    }

    pub fn read_timeout(
        &self,
        fid: Fid,
        offset: u64,
        count: u32,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        self.inner
            .read_timeout(fid, offset, count, timeout)
            .map_err(client_error)
    }

    pub(crate) fn submit_read(&self, fid: Fid, offset: u64, count: u32) -> Result<PendingRead> {
        self.inner
            .submit_read(fid, offset, count)
            .map_err(client_error)
    }

    pub(crate) fn wait_read_timeout(
        &self,
        pending: PendingRead,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        self.inner
            .wait_read_timeout(pending, timeout)
            .map_err(client_error)
    }

    pub(crate) fn flush_tag_timeout(&self, tag: Tag, timeout: Duration) -> Result<()> {
        self.inner
            .flush_tag_timeout(tag, timeout)
            .map_err(client_error)
    }

    pub fn read(&self, fid: Fid, offset: u64, count: u32) -> Result<Vec<u8>> {
        self.inner.read(fid, offset, count).map_err(client_error)
    }

    pub fn read_full_timeout(
        &self,
        fid: Fid,
        offset: u64,
        count: u32,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        self.inner
            .read_full_timeout(fid, offset, count, timeout)
            .map_err(client_error)
    }

    pub fn read_delimited_timeout(
        &self,
        fid: Fid,
        offset: u64,
        count: u32,
        delimiter: u8,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        self.inner
            .read_delimited_timeout(fid, offset, count, delimiter, timeout)
            .map_err(client_error)
    }

    /// Reads until the requested byte bound, EOF, or transport failure without
    /// imposing a response deadline. This is intended for namespace files
    /// whose read itself is the blocking subscription contract.
    pub fn read_full(&self, fid: Fid, offset: u64, count: u32) -> Result<Vec<u8>> {
        self.inner
            .read_full(fid, offset, count)
            .map_err(client_error)
    }

    /// Reads one bounded delimiter-terminated record without imposing a
    /// response deadline or issuing an EOF probe after the delimiter.
    pub fn read_delimited(
        &self,
        fid: Fid,
        offset: u64,
        count: u32,
        delimiter: u8,
    ) -> Result<Vec<u8>> {
        self.inner
            .read_delimited(fid, offset, count, delimiter)
            .map_err(client_error)
    }

    pub fn write_timeout(
        &self,
        fid: Fid,
        offset: u64,
        data: &[u8],
        timeout: Duration,
    ) -> Result<u32> {
        self.inner
            .write_timeout(fid, offset, data, timeout)
            .map_err(client_error)
    }

    pub fn write(&self, fid: Fid, offset: u64, data: &[u8]) -> Result<u32> {
        self.inner.write(fid, offset, data).map_err(client_error)
    }

    pub fn write_once(&self, fid: Fid, offset: u64, data: &[u8]) -> Result<u32> {
        self.inner
            .write_once(fid, offset, data)
            .map_err(client_error)
    }

    pub fn write_once_timeout(
        &self,
        fid: Fid,
        offset: u64,
        data: &[u8],
        timeout: Duration,
    ) -> Result<u32> {
        self.inner
            .write_once_timeout(fid, offset, data, timeout)
            .map_err(client_error)
    }

    pub fn write_then_read_delimited_timeout(
        &self,
        fid: Fid,
        write_offset: u64,
        data: &[u8],
        read: DelimitedRead,
        timeout: Duration,
    ) -> std::result::Result<(u32, Vec<u8>), WriteThenReadError> {
        self.inner
            .write_then_read_delimited_timeout(fid, write_offset, data, read, timeout)
            .map_err(WriteThenReadError::from)
    }

    pub fn clunk_timeout(&self, fid: Fid, timeout: Duration) -> Result<()> {
        self.inner.clunk_timeout(fid, timeout).map_err(client_error)
    }

    pub fn clunk(&self, fid: Fid) -> Result<()> {
        self.inner.clunk(fid).map_err(client_error)
    }

    /// Closes the shared transport and interrupts every pending call on this
    /// session connection.
    pub fn shutdown(&self) -> Result<()> {
        self.inner.shutdown().map_err(client_error)
    }

    pub fn remove_timeout(&self, fid: Fid, timeout: Duration) -> Result<()> {
        self.inner
            .remove_timeout(fid, timeout)
            .map_err(client_error)
    }

    pub fn remove(&self, fid: Fid) -> Result<()> {
        self.inner.remove(fid).map_err(client_error)
    }

    pub fn stat(&self, fid: Fid) -> Result<Stat> {
        let stat = self.inner.stat(fid).map_err(client_error)?;
        self.validate_stat(stat)
    }

    pub fn stat_timeout(&self, fid: Fid, timeout: Duration) -> Result<Stat> {
        let stat = self
            .inner
            .stat_timeout(fid, timeout)
            .map_err(client_error)?;
        self.validate_stat(stat)
    }

    pub fn wstat_timeout(&self, fid: Fid, stat: Stat, timeout: Duration) -> Result<()> {
        self.inner
            .wstat_timeout(fid, stat, timeout)
            .map_err(client_error)
    }

    pub fn wstat(&self, fid: Fid, stat: Stat) -> Result<()> {
        self.inner.wstat(fid, stat).map_err(client_error)
    }

    pub(crate) fn validate_stat(&self, stat: Stat) -> Result<Stat> {
        if !self.variant().supports_symlinks()
            && (stat.qid.is_symlink() || stat.mode & r9p::qid::DMSYMLINK != 0)
        {
            return Err(Error::new(
                libc::EPROTO,
                "server exposed symlink metadata without negotiating 9P2000.R",
            ));
        }
        Ok(stat)
    }
}

fn connect_should_retry(error: &Error, timeout: Duration, started: Instant) -> bool {
    !timeout.is_zero() && started.elapsed() < timeout && connect_error_is_transient(error)
}

fn connect_error_is_transient(error: &Error) -> bool {
    error.is_transient_connection_failure()
}

fn connect_retry_sleep(timeout: Duration, started: Instant) -> Duration {
    let remaining = timeout.saturating_sub(started.elapsed());
    CONNECT_RETRY_INTERVAL.min(remaining)
}
