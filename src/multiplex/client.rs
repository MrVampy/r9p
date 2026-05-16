use crate::{
    blocking::{parse_tcp_address, path_names},
    client::{Client as ProtocolClient, ClientResponse, Completion, Op},
    codec,
    error::{Error, Result},
    fid::Fid,
    message::{TMessage, Tag, NOTAG},
    qid::Qid,
    stat::Stat,
};
use std::{
    collections::BTreeMap,
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

use super::{
    reader::{call_message_sync, call_op_sync, reader_loop, ReplyResult, Waiters},
    util::{io_error, lock, op_fid, protocol_error, unexpected},
    MultiplexTransport,
};

pub struct MultiplexedClient<S: MultiplexTransport> {
    inner: Arc<MultiplexedInner<S>>,
}

impl<S: MultiplexTransport> Clone for MultiplexedClient<S> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

struct MultiplexedInner<S: MultiplexTransport> {
    protocol: Arc<Mutex<ProtocolClient>>,
    waiters: Arc<Mutex<Waiters>>,
    writer: Mutex<S>,
    reader: Mutex<Option<JoinHandle<()>>>,
    root_fid: Fid,
    root_qid: Qid,
}

pub struct PendingCall {
    tag: Tag,
    receiver: Receiver<ReplyResult>,
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

impl MultiplexedClient<TcpStream> {
    pub fn connect_tcp(address: &str, uname: &str, aname: &str, msize: u32) -> Result<Self> {
        let socket = parse_tcp_address(address)?;
        let stream = TcpStream::connect(&socket)
            .map_err(|error| io_error(format!("connect {socket}"), error))?;
        stream
            .set_nodelay(true)
            .map_err(|error| io_error("set TCP_NODELAY", error))?;
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
    pub fn connect(mut stream: S, uname: &str, aname: &str, msize: u32) -> Result<Self> {
        let mut reader = stream
            .try_clone_transport()
            .map_err(|error| io_error("clone 9P stream", error))?;
        let mut protocol = ProtocolClient::new();

        let version_request = protocol.version_request(msize);
        match call_message_sync(&mut stream, &mut reader, &mut protocol, version_request)? {
            ClientResponse::Completion {
                completion: Completion::Version { version, .. },
                ..
            } if version == b"9P2000" => {}
            ClientResponse::Completion {
                completion: Completion::Version { version, .. },
                ..
            } => {
                return Err(Error::from(format!(
                    "server negotiated unsupported version {}",
                    String::from_utf8_lossy(&version)
                )));
            }
            other => return Err(unexpected("Rversion", other)),
        }

        let attach = protocol
            .attach(uname.as_bytes().to_vec(), aname.as_bytes().to_vec())
            .map_err(protocol_error)?;
        let root_fid = op_fid(&attach)?;
        let root_qid = match call_op_sync(&mut stream, &mut reader, &mut protocol, attach)? {
            Completion::Attach { qid } => qid,
            other => return Err(unexpected("Rattach", other)),
        };

        let protocol = Arc::new(Mutex::new(protocol));
        let waiters = Arc::new(Mutex::new(BTreeMap::new()));
        let reader_protocol = Arc::clone(&protocol);
        let reader_waiters = Arc::clone(&waiters);
        let handle = thread::spawn(move || reader_loop(reader, reader_protocol, reader_waiters));

        Ok(Self {
            inner: Arc::new(MultiplexedInner {
                protocol,
                waiters,
                writer: Mutex::new(stream),
                reader: Mutex::new(Some(handle)),
                root_fid,
                root_qid,
            }),
        })
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

        let frame = codec::encode_tmessage(&message)
            .map_err(|error| Error::from(format!("encode 9P frame: {error}")))?;
        let (sender, receiver) = mpsc::channel();
        {
            let mut waiters = lock(&self.inner.waiters, "lock 9P waiter table")?;
            if waiters.insert(tag, sender).is_some() {
                return Err(Error::from(format!("duplicate waiter for tag {tag}")));
            }
        }

        let write_result = lock(&self.inner.writer, "lock 9P writer")?
            .write_all(&frame)
            .map_err(|error| io_error("write 9P frame", error));
        if let Err(error) = write_result {
            let _ = lock(&self.inner.waiters, "lock 9P waiter table")
                .map(|mut waiters| waiters.remove(&tag));
            return Err(error);
        }

        Ok(PendingCall { tag, receiver })
    }

    pub fn flush_tag(&self, oldtag: Tag) -> Result<()> {
        let op = {
            let mut protocol = lock(&self.inner.protocol, "lock 9P protocol client")?;
            protocol.flush(oldtag).map_err(protocol_error)?
        };
        match self.call_op(op)? {
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

    pub fn walk(&self, fid: Fid, names: &[Vec<u8>]) -> Result<Fid> {
        let op = {
            let mut protocol = lock(&self.inner.protocol, "lock 9P protocol client")?;
            protocol.walk(fid, names.to_vec()).map_err(protocol_error)?
        };
        let newfid = op_fid(&op)?;
        match self.call_op(op)? {
            Completion::Walk { qids } if qids.len() == names.len() => Ok(newfid),
            Completion::Walk { .. } => {
                let _ = self.clunk(newfid);
                Err(Error::from("partial walk"))
            }
            other => {
                let _ = self.clunk(newfid);
                Err(unexpected("Rwalk", other))
            }
        }
    }

    pub fn walk_timeout(&self, fid: Fid, names: &[Vec<u8>], timeout: Duration) -> Result<Fid> {
        let op = {
            let mut protocol = lock(&self.inner.protocol, "lock 9P protocol client")?;
            protocol.walk(fid, names.to_vec()).map_err(protocol_error)?
        };
        let newfid = op_fid(&op)?;
        match self.call_op_timeout(op, timeout)? {
            Completion::Walk { qids } if qids.len() == names.len() => Ok(newfid),
            Completion::Walk { .. } => {
                let _ = self.clunk_timeout(newfid, timeout);
                Err(Error::from("partial walk"))
            }
            other => {
                let _ = self.clunk_timeout(newfid, timeout);
                Err(unexpected("Rwalk", other))
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
            Completion::Read { data } => Ok(data),
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
        let count = codec::clamp_read_count(self.msize(), count);
        let op = {
            let mut protocol = lock(&self.inner.protocol, "lock 9P protocol client")?;
            protocol.read(fid, offset, count).map_err(protocol_error)?
        };
        match self.call_op_timeout(op, timeout)? {
            Completion::Read { data } => Ok(data),
            other => Err(unexpected("Rread", other)),
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
            offset = offset.saturating_add(u64::from(n));
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
            offset = offset.saturating_add(u64::from(n));
            remaining = remaining.saturating_sub(n);
        }
        Ok(out)
    }

    pub fn write(&self, fid: Fid, mut offset: u64, mut data: &[u8]) -> Result<u32> {
        if data.is_empty() {
            return self.write_once(fid, offset, data);
        }

        let mut total = 0_u32;
        let max = usize::try_from(self.max_write_payload()).unwrap_or(usize::MAX);
        while !data.is_empty() {
            let chunk_len = data.len().min(max);
            let chunk = &data[..chunk_len];
            let count = self.write_once(fid, offset, chunk)?;
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

    pub fn write_once(&self, fid: Fid, offset: u64, data: &[u8]) -> Result<u32> {
        let op = {
            let mut protocol = lock(&self.inner.protocol, "lock 9P protocol client")?;
            protocol
                .write(fid, offset, data.to_vec())
                .map_err(protocol_error)?
        };
        match self.call_op(op)? {
            Completion::Write { count } => Ok(count),
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

    fn call_op(&self, op: Op) -> Result<Completion> {
        let expected_tag = op.tag;
        match self.submit_op(op)?.wait()? {
            ClientResponse::Completion { tag, completion } if tag == expected_tag => Ok(completion),
            ClientResponse::Error { tag, ename } if tag == expected_tag => Err(Error::new(ename)),
            other => Err(Error::from(format!(
                "response tag mismatch or unexpected response: {other:?}"
            ))),
        }
    }

    fn call_op_timeout(&self, op: Op, timeout: Duration) -> Result<Completion> {
        let expected_tag = op.tag;
        let pending = self.submit_op(op)?;
        let response = match pending.receiver.recv_timeout(timeout) {
            Ok(response) => response?,
            Err(RecvTimeoutError::Timeout) => {
                self.cancel_waiter(
                    expected_tag,
                    Error::from(format!(
                        "9P response timeout after {:.3}s",
                        timeout.as_secs_f64()
                    )),
                );
                return Err(Error::from(format!(
                    "9P response timeout after {:.3}s",
                    timeout.as_secs_f64()
                )));
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(Error::from("9P reader stopped before response"));
            }
        };
        match response {
            ClientResponse::Completion { tag, completion } if tag == expected_tag => Ok(completion),
            ClientResponse::Error { tag, ename } if tag == expected_tag => Err(Error::new(ename)),
            other => Err(Error::from(format!(
                "response tag mismatch or unexpected response: {other:?}"
            ))),
        }
    }

    fn cancel_waiter(&self, tag: Tag, error: Error) {
        let sender = lock(&self.inner.waiters, "lock 9P waiter table")
            .ok()
            .and_then(|mut waiters| waiters.remove(&tag));
        if let Some(sender) = sender {
            let _ = sender.send(Err(error));
        }
    }
}

impl<S: MultiplexTransport> Drop for MultiplexedInner<S> {
    fn drop(&mut self) {
        if let Ok(writer) = self.writer.lock() {
            let _ = writer.shutdown_transport();
        }
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
