use crate::{
    error::client_error, request::RequestTracker, transport::connect_stream, Error, Result,
};
use r9p::{blocking, fid::Fid, multiplex::MultiplexedClient, qid::Qid, stat::Stat};
use std::{
    thread,
    time::{Duration, Instant},
};

const CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(200);

pub const OREAD: u8 = blocking::OREAD;
pub const OWRITE: u8 = blocking::OWRITE;
pub const ORDWR: u8 = blocking::ORDWR;
pub const OTRUNC: u8 = blocking::OTRUNC;

#[derive(Clone)]
pub struct Client {
    inner: MultiplexedClient<crate::transport::ClientStream>,
    tracker: RequestTracker,
}

impl Client {
    pub fn connect_with_timeout(
        address: &str,
        uname: &str,
        aname: &str,
        msize: u32,
        timeout: Duration,
    ) -> Result<Self> {
        Self::connect_with_tracker_timeout(
            address,
            uname,
            aname,
            msize,
            RequestTracker::default(),
            timeout,
        )
    }

    pub fn connect_with_tracker(
        address: &str,
        uname: &str,
        aname: &str,
        msize: u32,
        tracker: RequestTracker,
    ) -> Result<Self> {
        Self::connect_with_tracker_timeout(address, uname, aname, msize, tracker, Duration::ZERO)
    }

    pub fn connect_with_tracker_timeout(
        address: &str,
        uname: &str,
        aname: &str,
        msize: u32,
        tracker: RequestTracker,
        timeout: Duration,
    ) -> Result<Self> {
        let started = Instant::now();
        loop {
            match Self::connect_with_tracker_once(address, uname, aname, msize, tracker.clone()) {
                Ok(client) => return Ok(client),
                Err(error) if connect_should_retry(&error, timeout, started) => {
                    thread::sleep(connect_retry_sleep(timeout, started));
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn connect_with_tracker_once(
        address: &str,
        uname: &str,
        aname: &str,
        msize: u32,
        tracker: RequestTracker,
    ) -> Result<Self> {
        let stream = connect_stream(address)?;
        let inner =
            MultiplexedClient::connect(stream, uname, aname, msize).map_err(client_error)?;
        let tracked_inner = inner.clone();
        let tracked_requests = tracker.clone();
        let inner = inner.with_call_observer(move |tag| -> Box<dyn Send> {
            Box::new(tracked_requests.track_current(tag, tracked_inner.clone()))
        });
        Ok(Self { inner, tracker })
    }

    pub fn tracker(&self) -> RequestTracker {
        self.tracker.clone()
    }

    pub fn interrupt_fuse_unique(&self, unique: u64, timeout: Duration) -> Result<usize> {
        self.tracker.interrupt(unique, timeout)
    }

    pub fn root_fid(&self) -> Fid {
        self.inner.root_fid()
    }

    pub fn msize(&self) -> u32 {
        self.inner.msize()
    }

    pub fn max_write_payload(&self) -> u32 {
        self.inner.max_write_payload()
    }

    pub fn clone_fid_timeout(&self, fid: Fid, timeout: Duration) -> Result<Fid> {
        self.inner
            .clone_fid_timeout(fid, timeout)
            .map_err(client_error)
    }

    pub fn walk_one_timeout(&self, fid: Fid, name: &[u8], timeout: Duration) -> Result<Fid> {
        self.inner
            .walk_one_timeout(fid, name, timeout)
            .map_err(client_error)
    }

    pub fn walk_timeout(&self, fid: Fid, names: &[Vec<u8>], timeout: Duration) -> Result<Fid> {
        self.inner
            .walk_timeout(fid, names, timeout)
            .map_err(client_error)
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

    pub fn clunk_timeout(&self, fid: Fid, timeout: Duration) -> Result<()> {
        self.inner.clunk_timeout(fid, timeout).map_err(client_error)
    }

    pub fn remove_timeout(&self, fid: Fid, timeout: Duration) -> Result<()> {
        self.inner
            .remove_timeout(fid, timeout)
            .map_err(client_error)
    }

    pub fn stat_timeout(&self, fid: Fid, timeout: Duration) -> Result<Stat> {
        self.inner.stat_timeout(fid, timeout).map_err(client_error)
    }

    pub fn wstat_timeout(&self, fid: Fid, stat: Stat, timeout: Duration) -> Result<()> {
        self.inner
            .wstat_timeout(fid, stat, timeout)
            .map_err(client_error)
    }
}

fn connect_should_retry(error: &Error, timeout: Duration, started: Instant) -> bool {
    !timeout.is_zero() && started.elapsed() < timeout && connect_error_is_transient(error)
}

fn connect_error_is_transient(error: &Error) -> bool {
    matches!(
        error.errno,
        libc::ENOENT
            | libc::ECONNREFUSED
            | libc::ECONNRESET
            | libc::ECONNABORTED
            | libc::EAGAIN
            | libc::ETIMEDOUT
    )
}

fn connect_retry_sleep(timeout: Duration, started: Instant) -> Duration {
    let remaining = timeout.saturating_sub(started.elapsed());
    CONNECT_RETRY_INTERVAL.min(remaining)
}

#[cfg(test)]
mod tests;
