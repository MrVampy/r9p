use crate::{
    blocking::{connect_tcp_stream, path_names},
    client::{Client as ProtocolClient, ClientResponse, Completion, Op},
    client_support::{
        checked_advance_offset, checked_read_data, checked_write_count, io_error, op_fid,
        partial_walk, protocol_error, read_delimited_with, unexpected, write_in_chunks,
    },
    codec,
    error::{Error, Result},
    fid::Fid,
    message::{TMessage, Tag, NOTAG},
    qid::Qid,
    referral::NamespaceReferral,
    stat::Stat,
};
use std::{
    fmt,
    net::TcpStream,
    sync::{
        mpsc::{self, Receiver, RecvTimeoutError},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

#[cfg(unix)]
use std::{os::unix::net::UnixStream, path::Path};

type CallObserver = dyn Fn(Tag) -> Box<dyn Send> + Send + Sync + 'static;
type CallObserverGuard = Box<dyn Send>;

mod calls;

use super::{
    reader::{call_message_sync, call_op_sync, reader_loop, ReplyResult, ResponseState},
    util::{fail_all, lock},
    MultiplexTransport,
};

pub struct MultiplexedClient<S: MultiplexTransport> {
    inner: Arc<MultiplexedInner<S>>,
    call_observer: Option<Arc<CallObserver>>,
}

impl<S: MultiplexTransport> Clone for MultiplexedClient<S> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            call_observer: self.call_observer.clone(),
        }
    }
}

struct MultiplexedInner<S: MultiplexTransport> {
    protocol: Arc<Mutex<ProtocolClient>>,
    variant: codec::Variant,
    responses: Arc<Mutex<ResponseState>>,
    writer: Mutex<S>,
    reader: Mutex<Option<JoinHandle<()>>>,
    root_fid: Fid,
    root_qid: Qid,
}

pub struct PendingCall {
    tag: Tag,
    receiver: Receiver<ReplyResult>,
}

pub struct PendingRead {
    call: PendingCall,
    requested: u32,
    _observer: CallObserverGuard,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteThenReadError {
    Rejected(Error),
    DeliveryUnknown(Error),
}

impl WriteThenReadError {
    pub fn into_error(self) -> Error {
        match self {
            Self::Rejected(error) | Self::DeliveryUnknown(error) => error,
        }
    }
}

impl fmt::Display for WriteThenReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rejected(error) => write!(formatter, "9P write rejected: {error}"),
            Self::DeliveryUnknown(error) => {
                write!(formatter, "9P write delivery unknown: {error}")
            }
        }
    }
}

impl std::error::Error for WriteThenReadError {}

impl From<WriteThenReadError> for Error {
    fn from(error: WriteThenReadError) -> Self {
        error.into_error()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DelimitedRead {
    offset: u64,
    count: u32,
    delimiter: u8,
}

impl DelimitedRead {
    pub const fn new(offset: u64, count: u32, delimiter: u8) -> Self {
        Self {
            offset,
            count,
            delimiter,
        }
    }
}

impl PendingCall {
    pub fn tag(&self) -> Tag {
        self.tag
    }

    pub fn wait(self) -> Result<ClientResponse> {
        self.receiver
            .recv()
            .map_err(|_| Error::from("9P reader stopped before response"))?
    }
}

impl PendingRead {
    pub fn tag(&self) -> Tag {
        self.call.tag()
    }
}

impl MultiplexedClient<TcpStream> {
    pub fn connect_tcp(address: &str, uname: &str, aname: &str, msize: u32) -> Result<Self> {
        let stream = connect_tcp_stream(address)?;
        Self::connect(stream, uname, aname, msize)
    }
}

#[cfg(unix)]
impl MultiplexedClient<UnixStream> {
    pub fn connect_unix(path: &Path, uname: &str, aname: &str, msize: u32) -> Result<Self> {
        let stream = UnixStream::connect(path)
            .map_err(|error| io_error(format!("connect {}", path.display()), error))?;
        Self::connect(stream, uname, aname, msize)
    }
}

impl<S: MultiplexTransport> MultiplexedClient<S> {
    pub fn connect(stream: S, uname: &str, aname: &str, msize: u32) -> Result<Self> {
        Self::connect_with_variant(stream, uname, aname, msize, codec::Variant::Plain)
    }

    pub fn connect_with_variant(
        mut stream: S,
        uname: &str,
        aname: &str,
        msize: u32,
        requested_variant: codec::Variant,
    ) -> Result<Self> {
        let mut reader = stream
            .try_clone_transport()
            .map_err(|error| io_error("clone 9P stream", error))?;
        let mut protocol = ProtocolClient::new();

        let version_request = protocol.version_request_for(msize, requested_variant);
        let negotiated_variant =
            match call_message_sync(&mut stream, &mut reader, &mut protocol, version_request)? {
                ClientResponse::Completion {
                    completion: Completion::Version { version, .. },
                    ..
                } => requested_variant.accept_response(&version).ok_or_else(|| {
                    Error::from(format!(
                        "server negotiated unsupported version {}",
                        String::from_utf8_lossy(&version)
                    ))
                })?,
                other => return Err(unexpected("Rversion", other)),
            };

        let attach = protocol
            .attach(uname.as_bytes().to_vec(), aname.as_bytes().to_vec())
            .map_err(protocol_error)?;
        let root_fid = op_fid(&attach)?;
        let root_qid = match call_op_sync(&mut stream, &mut reader, &mut protocol, attach)? {
            Completion::Attach { qid } => qid,
            other => return Err(unexpected("Rattach", other)),
        };

        let protocol = Arc::new(Mutex::new(protocol));
        let responses = Arc::new(Mutex::new(ResponseState::default()));
        let reader_protocol = Arc::clone(&protocol);
        let reader_responses = Arc::clone(&responses);
        let handle = thread::spawn(move || reader_loop(reader, reader_protocol, reader_responses));

        Ok(Self {
            inner: Arc::new(MultiplexedInner {
                protocol,
                variant: negotiated_variant,
                responses,
                writer: Mutex::new(stream),
                reader: Mutex::new(Some(handle)),
                root_fid,
                root_qid,
            }),
            call_observer: None,
        })
    }

    pub fn variant(&self) -> codec::Variant {
        self.inner.variant
    }

    pub fn with_call_observer<F>(&self, observer: F) -> Self
    where
        F: Fn(Tag) -> Box<dyn Send> + Send + Sync + 'static,
    {
        Self {
            inner: Arc::clone(&self.inner),
            call_observer: Some(Arc::new(observer)),
        }
    }

    pub fn root_fid(&self) -> Fid {
        self.inner.root_fid
    }

    pub fn root_qid(&self) -> Qid {
        self.inner.root_qid
    }

    pub fn msize(&self) -> u32 {
        self.inner
            .protocol
            .lock()
            .map(|protocol| protocol.msize())
            .unwrap_or(codec::DEFAULT_MSIZE)
    }

    pub fn version(&self) -> Vec<u8> {
        self.inner
            .protocol
            .lock()
            .map(|protocol| protocol.version().to_vec())
            .unwrap_or_else(|_| b"9P2000".to_vec())
    }

    pub fn max_write_payload(&self) -> u32 {
        codec::max_write_payload(self.msize()).max(1)
    }

    /// Shuts down this client's shared transport, interrupting every pending
    /// call on the connection.
    pub fn shutdown(&self) -> Result<()> {
        let result = lock(&self.inner.writer, "lock 9P writer").and_then(|writer| {
            writer
                .shutdown_transport()
                .map_err(|error| io_error("shutdown 9P transport", error))
        });
        fail_all(
            &self.inner.responses,
            Error::from("9P transport closed by client"),
        );
        result
    }

    pub fn submit_op(&self, op: Op) -> Result<PendingCall> {
        self.submit_message(op.message)
    }

    pub fn submit<F>(&self, build: F) -> Result<PendingCall>
    where
        F: FnOnce(&mut ProtocolClient) -> Result<Op>,
    {
        let op = {
            let mut protocol = lock(&self.inner.protocol, "lock 9P protocol client")?;
            build(&mut protocol).map_err(protocol_error)?
        };
        self.submit_op(op)
    }

    pub fn submit_message(&self, message: TMessage) -> Result<PendingCall> {
        let tag = message.tag();
        if tag == NOTAG {
            return Err(Error::from("multiplexed calls require a real tag"));
        }

        let max_frame_size = lock(&self.inner.protocol, "lock 9P protocol client")?.msize();
        let frame = match codec::encode_tmessage_checked(&message, max_frame_size) {
            Ok(frame) => frame,
            Err(error) => {
                lock(&self.inner.protocol, "lock 9P protocol client")?.abandon(tag);
                return Err(Error::from(format!("encode 9P frame: {error}")));
            }
        };
        let (sender, receiver) = mpsc::channel();
        if let Err(error) = lock(&self.inner.responses, "lock 9P response state")
            .and_then(|mut responses| responses.register(tag, sender))
        {
            let _ = lock(&self.inner.protocol, "lock 9P protocol client")
                .map(|mut protocol| protocol.abandon(tag));
            return Err(error);
        }

        let write_result = lock(&self.inner.writer, "lock 9P writer").and_then(|mut writer| {
            writer
                .write_all(&frame)
                .map_err(|error| io_error("write 9P frame", error))
        });
        if let Err(error) = write_result {
            fail_all(&self.inner.responses, error.clone());
            let _ = lock(&self.inner.protocol, "lock 9P protocol client")
                .map(|mut protocol| protocol.abandon(tag));
            return Err(error);
        }

        Ok(PendingCall { tag, receiver })
    }

    pub fn flush_tag(&self, oldtag: Tag) -> Result<()> {
        self.flush_tag_with(oldtag, None)
    }

    pub fn flush_tag_timeout(&self, oldtag: Tag, timeout: Duration) -> Result<()> {
        self.flush_tag_with(oldtag, Some(timeout))
    }

    fn flush_tag_with(&self, oldtag: Tag, timeout: Option<Duration>) -> Result<()> {
        let op = {
            let mut protocol = lock(&self.inner.protocol, "lock 9P protocol client")?;
            protocol.flush(oldtag).map_err(protocol_error)?
        };
        let completion = match timeout {
            Some(timeout) => self.call_op_timeout_inner(op, timeout, |_| {}, false)?,
            None => self.call_op(op)?,
        };
        match completion {
            Completion::Flush => {
                self.cancel_waiter(oldtag, Error::from("9P request flushed"));
                Ok(())
            }
            other => Err(unexpected("Rflush", other)),
        }
    }

    pub fn clone_fid(&self, fid: Fid) -> Result<Fid> {
        self.walk(fid, &[])
    }

    pub fn clone_fid_timeout(&self, fid: Fid, timeout: Duration) -> Result<Fid> {
        self.walk_timeout(fid, &[], timeout)
    }

    pub fn walk_path(&self, path: &str) -> Result<Fid> {
        let names = path_names(path);
        if names.is_empty() {
            return self.clone_fid(self.root_fid());
        }
        self.walk(self.root_fid(), &names)
    }

    pub fn walk_one(&self, fid: Fid, name: &[u8]) -> Result<Fid> {
        self.walk(fid, &[name.to_vec()])
    }

    pub fn walk_one_timeout(&self, fid: Fid, name: &[u8], timeout: Duration) -> Result<Fid> {
        self.walk_timeout(fid, &[name.to_vec()], timeout)
    }

    pub fn referrals_timeout(&self, fid: Fid, timeout: Duration) -> Result<Vec<NamespaceReferral>> {
        if !self.variant().supports_referrals() {
            return Err(Error::from("server did not negotiate namespace referrals"));
        }
        let op = {
            let mut protocol = lock(&self.inner.protocol, "lock 9P protocol client")?;
            protocol.referrals(fid).map_err(protocol_error)?
        };
        match self.call_op_timeout(op, timeout)? {
            Completion::Referrals { referrals } => Ok(referrals),
            other => Err(unexpected("Rreferrals", other)),
        }
    }

    pub fn walk(&self, fid: Fid, names: &[Vec<u8>]) -> Result<Fid> {
        match self.walk_short(fid, names, None)? {
            Ok(newfid) => Ok(newfid),
            Err(walked) => Err(partial_walk(
                names,
                walked,
                self.walk_refusal(fid, names, walked, None),
            )),
        }
    }

    pub fn walk_timeout(&self, fid: Fid, names: &[Vec<u8>], timeout: Duration) -> Result<Fid> {
        match self.walk_short(fid, names, Some(timeout))? {
            Ok(newfid) => Ok(newfid),
            Err(walked) => Err(partial_walk(
                names,
                walked,
                self.walk_refusal(fid, names, walked, Some(timeout)),
            )),
        }
    }

    /// `Err(walked)` reports how many elements the server did accept.
    fn walk_short(
        &self,
        fid: Fid,
        names: &[Vec<u8>],
        timeout: Option<Duration>,
    ) -> Result<std::result::Result<Fid, usize>> {
        let op = {
            let mut protocol = lock(&self.inner.protocol, "lock 9P protocol client")?;
            protocol.walk(fid, names.to_vec()).map_err(protocol_error)?
        };
        let newfid = op_fid(&op)?;
        let completion = match timeout {
            Some(timeout) => self.call_op_timeout(op, timeout)?,
            None => self.call_op(op)?,
        };
        match completion {
            Completion::Walk { qids } if qids.len() == names.len() => Ok(Ok(newfid)),
            Completion::Walk { qids } => {
                self.release(newfid, timeout);
                Ok(Err(qids.len()))
            }
            other => {
                self.release(newfid, timeout);
                Err(unexpected("Rwalk", other))
            }
        }
    }

    fn walk_refusal(
        &self,
        fid: Fid,
        names: &[Vec<u8>],
        walked: usize,
        timeout: Option<Duration>,
    ) -> Option<Error> {
        let stopped = names.get(walked)?.clone();
        let prefix = self.walk_short(fid, &names[..walked], timeout).ok()?.ok()?;
        let refusal = match self.walk_short(prefix, &[stopped], timeout) {
            Ok(Ok(reached)) => {
                self.release(reached, timeout);
                None
            }
            Ok(Err(_)) => None,
            Err(error) => Some(error),
        };
        self.release(prefix, timeout);
        refusal
    }

    fn release(&self, fid: Fid, timeout: Option<Duration>) {
        match timeout {
            Some(timeout) => {
                let _ = self.clunk_timeout(fid, timeout);
            }
            None => {
                let _ = self.clunk(fid);
            }
        }
    }

    pub fn open(&self, fid: Fid, mode: u8) -> Result<Qid> {
        let op = {
            let mut protocol = lock(&self.inner.protocol, "lock 9P protocol client")?;
            protocol.open(fid, mode).map_err(protocol_error)?
        };
        match self.call_op(op)? {
            Completion::Open { qid, .. } => Ok(qid),
            other => Err(unexpected("Ropen", other)),
        }
    }

    pub fn open_timeout(&self, fid: Fid, mode: u8, timeout: Duration) -> Result<Qid> {
        let op = {
            let mut protocol = lock(&self.inner.protocol, "lock 9P protocol client")?;
            protocol.open(fid, mode).map_err(protocol_error)?
        };
        match self.call_op_timeout(op, timeout)? {
            Completion::Open { qid, .. } => Ok(qid),
            other => Err(unexpected("Ropen", other)),
        }
    }

    pub fn create(&self, parent_fid: Fid, name: &[u8], perm: u32, mode: u8) -> Result<(Fid, Qid)> {
        let fid = self.clone_fid(parent_fid)?;
        let op = {
            let mut protocol = lock(&self.inner.protocol, "lock 9P protocol client")?;
            protocol
                .create(fid, name.to_vec(), perm, mode)
                .map_err(protocol_error)?
        };
        let reply = self.call_op(op);
        match reply {
            Ok(Completion::Create { qid, .. }) => Ok((fid, qid)),
            Ok(other) => {
                let _ = self.clunk(fid);
                Err(unexpected("Rcreate", other))
            }
            Err(error) => {
                let _ = self.clunk(fid);
                Err(error)
            }
        }
    }

    pub fn create_timeout(
        &self,
        parent_fid: Fid,
        name: &[u8],
        perm: u32,
        mode: u8,
        timeout: Duration,
    ) -> Result<(Fid, Qid)> {
        let fid = self.clone_fid_timeout(parent_fid, timeout)?;
        let op = {
            let mut protocol = lock(&self.inner.protocol, "lock 9P protocol client")?;
            protocol
                .create(fid, name.to_vec(), perm, mode)
                .map_err(protocol_error)?
        };
        let reply = self.call_op_timeout(op, timeout);
        match reply {
            Ok(Completion::Create { qid, .. }) => Ok((fid, qid)),
            Ok(other) => {
                let _ = self.clunk_timeout(fid, timeout);
                Err(unexpected("Rcreate", other))
            }
            Err(error) => {
                let _ = self.clunk_timeout(fid, timeout);
                Err(error)
            }
        }
    }

    pub fn read(&self, fid: Fid, offset: u64, count: u32) -> Result<Vec<u8>> {
        let count = codec::clamp_read_count(self.msize(), count);
        let op = {
            let mut protocol = lock(&self.inner.protocol, "lock 9P protocol client")?;
            protocol.read(fid, offset, count).map_err(protocol_error)?
        };
        match self.call_op(op)? {
            Completion::Read { data } => checked_read_data(data, count),
            other => Err(unexpected("Rread", other)),
        }
    }

    pub fn read_timeout(
        &self,
        fid: Fid,
        offset: u64,
        count: u32,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        let pending = self.submit_read(fid, offset, count)?;
        self.wait_read_timeout(pending, timeout)
    }

    /// Submits one positional read without waiting for its response.
    ///
    /// The returned tag can be flushed independently while other reads on the
    /// same fid remain in flight.
    pub fn submit_read(&self, fid: Fid, offset: u64, count: u32) -> Result<PendingRead> {
        let count = codec::clamp_read_count(self.msize(), count);
        let op = {
            let mut protocol = lock(&self.inner.protocol, "lock 9P protocol client")?;
            protocol.read(fid, offset, count).map_err(protocol_error)?
        };
        let tag = op.tag;
        let call = self.submit_op(op)?;
        let observer = self.observe_call(tag);
        Ok(PendingRead {
            call,
            requested: count,
            _observer: observer,
        })
    }

    pub fn wait_read_timeout(&self, pending: PendingRead, timeout: Duration) -> Result<Vec<u8>> {
        let requested = pending.requested;
        match self.wait_pending_timeout(pending.call, timeout, true)? {
            Completion::Read { data } => checked_read_data(data, requested),
            other => Err(unexpected("Rread", other)),
        }
    }

    /// Waits for a submitted read without imposing a response deadline.
    ///
    /// This is intended for blocking namespace subscriptions. Transport
    /// failure and an explicit `Tflush` still wake the waiter.
    pub fn wait_read(&self, pending: PendingRead) -> Result<Vec<u8>> {
        let requested = pending.requested;
        let expected_tag = pending.call.tag;
        match pending.call.wait()? {
            ClientResponse::Completion { tag, completion } if tag == expected_tag => {
                match completion {
                    Completion::Read { data } => checked_read_data(data, requested),
                    other => Err(unexpected("Rread", other)),
                }
            }
            ClientResponse::Error { tag, ename } if tag == expected_tag => Err(Error::new(ename)),
            other => Err(Error::from(format!(
                "response tag mismatch or unexpected response: {other:?}"
            ))),
        }
    }

    pub fn read_full(&self, fid: Fid, mut offset: u64, count: u32) -> Result<Vec<u8>> {
        let mut remaining = count;
        let mut out = Vec::with_capacity(usize::try_from(count).unwrap_or(0));
        while remaining > 0 {
            let data = self.read(fid, offset, remaining)?;
            if data.is_empty() {
                break;
            }
            let n = u32::try_from(data.len()).map_err(|_| Error::from("read count overflow"))?;
            out.extend(data);
            offset = checked_advance_offset(offset, u64::from(n))?;
            remaining = remaining.saturating_sub(n);
        }
        Ok(out)
    }

    pub fn read_full_timeout(
        &self,
        fid: Fid,
        mut offset: u64,
        count: u32,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        let mut remaining = count;
        let mut out = Vec::with_capacity(usize::try_from(count).unwrap_or(0));
        while remaining > 0 {
            let data = self.read_timeout(fid, offset, remaining, timeout)?;
            if data.is_empty() {
                break;
            }
            let n = u32::try_from(data.len()).map_err(|_| Error::from("read count overflow"))?;
            out.extend(data);
            offset = checked_advance_offset(offset, u64::from(n))?;
            remaining = remaining.saturating_sub(n);
        }
        Ok(out)
    }

    /// Reads one bounded delimiter-terminated record without probing for EOF
    /// after the delimiter has arrived.
    ///
    /// The delimiter is included in the returned bytes. A response chunk that
    /// contains bytes after the first delimiter is rejected because this
    /// stateless operation cannot retain them for a later record.
    pub fn read_delimited(
        &self,
        fid: Fid,
        offset: u64,
        count: u32,
        delimiter: u8,
    ) -> Result<Vec<u8>> {
        read_delimited_with(offset, count, delimiter, |offset, remaining| {
            self.read(fid, offset, remaining)
        })
    }

    /// Bounded variant of [`Self::read_delimited`] with a deadline for each
    /// underlying 9P read.
    pub fn read_delimited_timeout(
        &self,
        fid: Fid,
        offset: u64,
        count: u32,
        delimiter: u8,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        read_delimited_with(offset, count, delimiter, |offset, remaining| {
            self.read_timeout(fid, offset, remaining, timeout)
        })
    }

    pub fn write(&self, fid: Fid, offset: u64, data: &[u8]) -> Result<u32> {
        write_in_chunks(self.max_write_payload(), offset, data, |offset, chunk| {
            self.write_once(fid, offset, chunk)
        })
    }

    pub fn write_timeout(
        &self,
        fid: Fid,
        offset: u64,
        data: &[u8],
        timeout: Duration,
    ) -> Result<u32> {
        write_in_chunks(self.max_write_payload(), offset, data, |offset, chunk| {
            self.write_once_timeout(fid, offset, chunk, timeout)
        })
    }

    /// Sends the final write chunk and the following read without waiting for
    /// the write reply between them.
    ///
    /// The two requests are ordered on the wire. Callers must use this only
    /// with a file whose server contract processes a write before the
    /// subsequent read on the same fid. Prefix chunks still complete before
    /// the pipelined final pair.
    pub fn write_then_read_timeout(
        &self,
        fid: Fid,
        mut write_offset: u64,
        mut data: &[u8],
        read_offset: u64,
        read_count: u32,
        timeout: Duration,
    ) -> std::result::Result<(u32, Vec<u8>), WriteThenReadError> {
        let max = usize::try_from(self.max_write_payload()).unwrap_or(usize::MAX);
        let mut total = 0_u32;
        while data.len() > max {
            let chunk = &data[..max];
            let count = self
                .write_once_timeout(fid, write_offset, chunk, timeout)
                .map_err(WriteThenReadError::DeliveryUnknown)?;
            if usize::try_from(count).ok() != Some(chunk.len()) {
                return Err(WriteThenReadError::DeliveryUnknown(Error::from(
                    "short 9P write before pipelined read",
                )));
            }
            total = total.checked_add(count).ok_or_else(|| {
                WriteThenReadError::DeliveryUnknown(Error::from("aggregate write count overflow"))
            })?;
            write_offset = checked_advance_offset(write_offset, u64::from(count))
                .map_err(WriteThenReadError::DeliveryUnknown)?;
            data = &data[max..];
        }

        let (count, response) = self.write_once_then_read_timeout(
            fid,
            write_offset,
            data,
            read_offset,
            read_count,
            timeout,
        )?;
        total = total.checked_add(count).ok_or_else(|| {
            WriteThenReadError::DeliveryUnknown(Error::from("aggregate write count overflow"))
        })?;
        Ok((total, response))
    }

    /// Pipelined [`Self::write_then_read_timeout`] that reads one bounded
    /// delimiter-terminated response record.
    pub fn write_then_read_delimited_timeout(
        &self,
        fid: Fid,
        write_offset: u64,
        data: &[u8],
        read: DelimitedRead,
        timeout: Duration,
    ) -> std::result::Result<(u32, Vec<u8>), WriteThenReadError> {
        let (written, first) = self.write_then_read_timeout(
            fid,
            write_offset,
            data,
            read.offset,
            read.count,
            timeout,
        )?;
        let mut first = Some(first);
        let response = read_delimited_with(
            read.offset,
            read.count,
            read.delimiter,
            |offset, remaining| match first.take() {
                Some(bytes) => Ok(bytes),
                None => self.read_timeout(fid, offset, remaining, timeout),
            },
        )
        .map_err(WriteThenReadError::DeliveryUnknown)?;
        Ok((written, response))
    }

    pub fn write_once(&self, fid: Fid, offset: u64, data: &[u8]) -> Result<u32> {
        let op = {
            let mut protocol = lock(&self.inner.protocol, "lock 9P protocol client")?;
            protocol
                .write(fid, offset, data.to_vec())
                .map_err(protocol_error)?
        };
        match self.call_op(op)? {
            Completion::Write { count } => checked_write_count(count, data.len()),
            other => Err(unexpected("Rwrite", other)),
        }
    }

    fn write_once_then_read_timeout(
        &self,
        fid: Fid,
        write_offset: u64,
        data: &[u8],
        read_offset: u64,
        read_count: u32,
        timeout: Duration,
    ) -> std::result::Result<(u32, Vec<u8>), WriteThenReadError> {
        let read_count = codec::clamp_read_count(self.msize(), read_count);
        let (write, read) = {
            let mut protocol = lock(&self.inner.protocol, "lock 9P protocol client")
                .map_err(WriteThenReadError::Rejected)?;
            let write = protocol
                .write(fid, write_offset, data.to_vec())
                .map_err(protocol_error)
                .map_err(WriteThenReadError::Rejected)?;
            let read = match protocol.read(fid, read_offset, read_count) {
                Ok(read) => read,
                Err(error) => {
                    protocol.abandon(write.tag);
                    return Err(WriteThenReadError::Rejected(protocol_error(error)));
                }
            };
            (write, read)
        };
        let write_tag = write.tag;
        let read_tag = read.tag;
        let write_pending = match self.submit_op(write) {
            Ok(pending) => pending,
            Err(error) => {
                let _ = lock(&self.inner.protocol, "lock 9P protocol client")
                    .map(|mut protocol| protocol.abandon(read_tag));
                return Err(WriteThenReadError::DeliveryUnknown(error));
            }
        };
        let read_pending = match self.submit_op(read) {
            Ok(pending) => pending,
            Err(error) => {
                self.cancel_waiter(
                    write_tag,
                    Error::from("pipelined 9P read could not be submitted"),
                );
                return Err(WriteThenReadError::DeliveryUnknown(error));
            }
        };
        let _write_guard = self.observe_call(write_tag);
        let _read_guard = self.observe_call(read_tag);

        let write_result = self.wait_pending_response_timeout(write_pending, timeout, true);
        let read_result = self.wait_pending_response_timeout(read_pending, timeout, true);
        let written = match write_result {
            Ok(ClientResponse::Completion {
                tag,
                completion: Completion::Write { count },
            }) if tag == write_tag => checked_write_count(count, data.len())
                .map_err(WriteThenReadError::DeliveryUnknown)?,
            Ok(ClientResponse::Error { tag, ename }) if tag == write_tag => {
                return Err(WriteThenReadError::Rejected(Error::new(ename)));
            }
            Ok(other) => {
                return Err(WriteThenReadError::DeliveryUnknown(Error::from(format!(
                    "expected Rwrite, got {other:?}"
                ))));
            }
            Err(error) => return Err(WriteThenReadError::DeliveryUnknown(error)),
        };
        let response = match read_result {
            Ok(ClientResponse::Completion {
                tag,
                completion: Completion::Read { data },
            }) if tag == read_tag => {
                checked_read_data(data, read_count).map_err(WriteThenReadError::DeliveryUnknown)?
            }
            Ok(ClientResponse::Error { tag, ename }) if tag == read_tag => {
                return Err(WriteThenReadError::DeliveryUnknown(Error::new(ename)));
            }
            Ok(other) => {
                return Err(WriteThenReadError::DeliveryUnknown(Error::from(format!(
                    "expected Rread, got {other:?}"
                ))));
            }
            Err(error) => return Err(WriteThenReadError::DeliveryUnknown(error)),
        };
        Ok((written, response))
    }

    pub fn write_once_timeout(
        &self,
        fid: Fid,
        offset: u64,
        data: &[u8],
        timeout: Duration,
    ) -> Result<u32> {
        let op = {
            let mut protocol = lock(&self.inner.protocol, "lock 9P protocol client")?;
            protocol
                .write(fid, offset, data.to_vec())
                .map_err(protocol_error)?
        };
        match self.call_op_timeout(op, timeout)? {
            Completion::Write { count } => checked_write_count(count, data.len()),
            other => Err(unexpected("Rwrite", other)),
        }
    }

    pub fn clunk(&self, fid: Fid) -> Result<()> {
        let op = {
            let mut protocol = lock(&self.inner.protocol, "lock 9P protocol client")?;
            protocol.clunk(fid).map_err(protocol_error)?
        };
        match self.call_op(op)? {
            Completion::Clunk => Ok(()),
            other => Err(unexpected("Rclunk", other)),
        }
    }

    pub fn clunk_timeout(&self, fid: Fid, timeout: Duration) -> Result<()> {
        let op = {
            let mut protocol = lock(&self.inner.protocol, "lock 9P protocol client")?;
            protocol.clunk(fid).map_err(protocol_error)?
        };
        match self.call_op_timeout(op, timeout)? {
            Completion::Clunk => Ok(()),
            other => Err(unexpected("Rclunk", other)),
        }
    }

    pub fn remove(&self, fid: Fid) -> Result<()> {
        let op = {
            let mut protocol = lock(&self.inner.protocol, "lock 9P protocol client")?;
            protocol.remove(fid).map_err(protocol_error)?
        };
        match self.call_op(op)? {
            Completion::Remove => Ok(()),
            other => Err(unexpected("Rremove", other)),
        }
    }

    pub fn remove_timeout(&self, fid: Fid, timeout: Duration) -> Result<()> {
        let op = {
            let mut protocol = lock(&self.inner.protocol, "lock 9P protocol client")?;
            protocol.remove(fid).map_err(protocol_error)?
        };
        match self.call_op_timeout(op, timeout)? {
            Completion::Remove => Ok(()),
            other => Err(unexpected("Rremove", other)),
        }
    }

    pub fn stat(&self, fid: Fid) -> Result<Stat> {
        let op = {
            let mut protocol = lock(&self.inner.protocol, "lock 9P protocol client")?;
            protocol.stat(fid).map_err(protocol_error)?
        };
        match self.call_op(op)? {
            Completion::Stat { stat } => Ok(stat),
            other => Err(unexpected("Rstat", other)),
        }
    }

    pub fn stat_timeout(&self, fid: Fid, timeout: Duration) -> Result<Stat> {
        let op = {
            let mut protocol = lock(&self.inner.protocol, "lock 9P protocol client")?;
            protocol.stat(fid).map_err(protocol_error)?
        };
        match self.call_op_timeout(op, timeout)? {
            Completion::Stat { stat } => Ok(stat),
            other => Err(unexpected("Rstat", other)),
        }
    }

    pub fn wstat(&self, fid: Fid, stat: Stat) -> Result<()> {
        let op = {
            let mut protocol = lock(&self.inner.protocol, "lock 9P protocol client")?;
            protocol.wstat(fid, stat).map_err(protocol_error)?
        };
        match self.call_op(op)? {
            Completion::Wstat => Ok(()),
            other => Err(unexpected("Rwstat", other)),
        }
    }

    pub fn wstat_timeout(&self, fid: Fid, stat: Stat, timeout: Duration) -> Result<()> {
        let op = {
            let mut protocol = lock(&self.inner.protocol, "lock 9P protocol client")?;
            protocol.wstat(fid, stat).map_err(protocol_error)?
        };
        match self.call_op_timeout(op, timeout)? {
            Completion::Wstat => Ok(()),
            other => Err(unexpected("Rwstat", other)),
        }
    }
}

fn bounded_flush_timeout(timeout: Duration) -> Duration {
    timeout.min(Duration::from_millis(250))
}

impl<S: MultiplexTransport> Drop for MultiplexedInner<S> {
    fn drop(&mut self) {
        if let Ok(writer) = self.writer.lock() {
            let _ = writer.shutdown_transport();
        }
        fail_all(
            &self.responses,
            Error::from("9P transport closed by client"),
        );
        if let Ok(mut reader) = self.reader.lock() {
            if let Some(handle) = reader.take() {
                let _ = handle.join();
            }
        }
    }
}

#[cfg(test)]
pub(super) fn pending_for_test(tag: Tag, receiver: Receiver<ReplyResult>) -> PendingCall {
    PendingCall { tag, receiver }
}
