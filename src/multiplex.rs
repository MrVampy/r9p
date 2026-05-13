use crate::{
    blocking::{parse_tcp_address, path_names},
    client::{Client as ProtocolClient, ClientResponse, Completion, Op},
    codec,
    error::{Error, Result},
    fid::Fid,
    message::{RMessage, TMessage, Tag, NOTAG},
    qid::Qid,
    stat::Stat,
};
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    net::{Shutdown, TcpStream},
    sync::{
        mpsc::{self, Receiver, Sender},
        Arc, Mutex, MutexGuard,
    },
    thread::{self, JoinHandle},
};

#[cfg(unix)]
use std::{os::unix::net::UnixStream, path::Path};

type ReplyResult = std::result::Result<ClientResponse, Error>;
type Waiters = BTreeMap<Tag, Sender<ReplyResult>>;

pub trait MultiplexTransport: Read + Write + Send + 'static {
    fn try_clone_transport(&self) -> io::Result<Self>
    where
        Self: Sized;

    fn shutdown_transport(&self) -> io::Result<()>;
}

impl MultiplexTransport for TcpStream {
    fn try_clone_transport(&self) -> io::Result<Self> {
        self.try_clone()
    }

    fn shutdown_transport(&self) -> io::Result<()> {
        self.shutdown(Shutdown::Both)
    }
}

#[cfg(unix)]
impl MultiplexTransport for UnixStream {
    fn try_clone_transport(&self) -> io::Result<Self> {
        self.try_clone()
    }

    fn shutdown_transport(&self) -> io::Result<()> {
        self.shutdown(Shutdown::Both)
    }
}

#[derive(Clone)]
pub struct MultiplexedClient<S: MultiplexTransport> {
    inner: Arc<MultiplexedInner<S>>,
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

fn reader_loop<S: MultiplexTransport>(
    mut reader: S,
    protocol: Arc<Mutex<ProtocolClient>>,
    waiters: Arc<Mutex<Waiters>>,
) {
    loop {
        let response = match read_response(&mut reader) {
            Ok(response) => response,
            Err(error) => {
                fail_all(&waiters, error);
                return;
            }
        };
        let response = match lock(&protocol, "lock 9P protocol client")
            .and_then(|mut protocol| protocol.receive(response).map_err(protocol_error))
        {
            Ok(response) => response,
            Err(error) if error.message() == b"9P client state: unknown response tag" => continue,
            Err(error) => {
                fail_all(&waiters, error);
                return;
            }
        };
        let tag = response_tag(&response);
        let sender = match lock(&waiters, "lock 9P waiter table") {
            Ok(mut waiters) => waiters.remove(&tag),
            Err(error) => {
                fail_all(&waiters, error);
                return;
            }
        };
        if let Some(sender) = sender {
            let _ = sender.send(Ok(response));
        }
    }
}

fn call_op_sync<S: Read + Write>(
    writer: &mut S,
    reader: &mut S,
    protocol: &mut ProtocolClient,
    op: Op,
) -> Result<Completion> {
    let expected_tag = op.tag;
    match call_message_sync(writer, reader, protocol, op.message)? {
        ClientResponse::Completion { tag, completion } if tag == expected_tag => Ok(completion),
        ClientResponse::Error { tag, ename } if tag == expected_tag => Err(Error::new(ename)),
        other => Err(Error::from(format!(
            "response tag mismatch or unexpected response: {other:?}"
        ))),
    }
}

fn call_message_sync<S: Read + Write>(
    writer: &mut S,
    reader: &mut S,
    protocol: &mut ProtocolClient,
    message: TMessage,
) -> Result<ClientResponse> {
    let frame = codec::encode_tmessage(&message)
        .map_err(|error| Error::from(format!("encode 9P frame: {error}")))?;
    writer
        .write_all(&frame)
        .map_err(|error| io_error("write 9P frame", error))?;
    let response = read_response(reader)?;
    protocol.receive(response).map_err(protocol_error)
}

fn read_response(reader: &mut impl Read) -> Result<RMessage> {
    let mut prefix = [0_u8; 4];
    reader
        .read_exact(&mut prefix)
        .map_err(|error| io_error("read 9P frame size", error))?;
    let size = u32::from_le_bytes(prefix);
    if size < codec::FRAME_HEADER_SIZE {
        return Err(Error::from("short 9P frame"));
    }
    let rest_len = usize::try_from(size - 4).map_err(|_| Error::from("oversized 9P frame"))?;
    let mut frame = Vec::with_capacity(usize::try_from(size).unwrap_or(rest_len + 4));
    frame.extend(prefix);
    frame.resize(rest_len + 4, 0);
    reader
        .read_exact(&mut frame[4..])
        .map_err(|error| io_error("read 9P frame body", error))?;
    codec::decode_rmessage(&frame).map_err(|error| Error::from(format!("decode 9P frame: {error}")))
}

#[cfg(test)]
fn write_response(writer: &mut impl Write, message: &RMessage) -> Result<()> {
    let frame = codec::encode_rmessage(message)
        .map_err(|error| Error::from(format!("encode 9P frame: {error}")))?;
    writer
        .write_all(&frame)
        .map_err(|error| io_error("write 9P frame", error))
}

fn fail_all(waiters: &Mutex<Waiters>, error: Error) {
    if let Ok(mut waiters) = waiters.lock() {
        let pending = std::mem::take(&mut *waiters);
        for sender in pending.into_values() {
            let _ = sender.send(Err(error.clone()));
        }
    }
}

fn response_tag(response: &ClientResponse) -> Tag {
    match response {
        ClientResponse::Completion { tag, .. } | ClientResponse::Error { tag, .. } => *tag,
    }
}

fn op_fid(op: &Op) -> Result<Fid> {
    op.fid
        .ok_or_else(|| Error::from("9P operation did not allocate a fid"))
}

fn protocol_error(error: Error) -> Error {
    Error::from(format!("9P client state: {error}"))
}

fn io_error(context: impl AsRef<str>, error: io::Error) -> Error {
    Error::from(format!("{}: {error}", context.as_ref()))
}

fn unexpected(expected: &str, got: impl std::fmt::Debug) -> Error {
    Error::from(format!("expected {expected}, got {got:?}"))
}

fn lock<'a, T>(mutex: &'a Mutex<T>, context: &'static str) -> Result<MutexGuard<'a, T>> {
    mutex.lock().map_err(|_| Error::from(context))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{fid::NOFID, qid::Qid};
    use std::sync::{mpsc, Arc, Barrier};

    #[cfg(unix)]
    #[test]
    fn concurrent_calls_are_demultiplexed_by_tag() -> Result<()> {
        let (client_stream, server_stream) =
            UnixStream::pair().map_err(|error| io_error("create unix pair", error))?;
        let server = thread::spawn(move || scripted_out_of_order_server(server_stream));
        let client = Arc::new(MultiplexedClient::connect(
            client_stream,
            "glenda",
            "",
            8192,
        )?);
        let root = client.root_fid();
        let barrier = Arc::new(Barrier::new(3));

        let read_client = Arc::clone(&client);
        let read_barrier = Arc::clone(&barrier);
        let read_thread = thread::spawn(move || {
            read_barrier.wait();
            read_client.read(root, 0, 100)
        });

        let stat_client = Arc::clone(&client);
        let stat_barrier = Arc::clone(&barrier);
        let stat_thread = thread::spawn(move || {
            stat_barrier.wait();
            stat_client.stat(root)
        });

        barrier.wait();
        let stat = stat_thread
            .join()
            .map_err(|_| Error::from("stat worker panicked"))??;
        let data = read_thread
            .join()
            .map_err(|_| Error::from("read worker panicked"))??;
        assert_eq!(stat.name, b".".to_vec());
        assert_eq!(data, b"read after stat\n".to_vec());
        server
            .join()
            .map_err(|_| Error::from("server worker panicked"))??;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn flush_releases_original_waiter() -> Result<()> {
        let (client_stream, server_stream) =
            UnixStream::pair().map_err(|error| io_error("create unix pair", error))?;
        let (done_sender, done_receiver) = mpsc::channel();
        let server = thread::spawn(move || scripted_flush_server(server_stream, done_receiver));
        let client = MultiplexedClient::connect(client_stream, "glenda", "", 8192)?;
        let pending = client.submit(|protocol| protocol.read(client.root_fid(), 0, 100))?;
        let oldtag = pending.tag();
        client.flush_tag(oldtag)?;
        let error = pending.wait().err().ok_or("flushed request completed")?;
        assert_eq!(error.message(), b"9P request flushed");
        done_sender
            .send(())
            .map_err(|_| Error::from("flush server stopped early"))?;
        server
            .join()
            .map_err(|_| Error::from("server worker panicked"))??;
        Ok(())
    }

    #[cfg(unix)]
    fn scripted_out_of_order_server(mut stream: UnixStream) -> Result<()> {
        handshake(&mut stream)?;

        let first = read_tmessage(&mut stream)?;
        let second = read_tmessage(&mut stream)?;
        let mut read_tag = None;
        let mut stat_tag = None;
        for message in [first, second] {
            match message {
                TMessage::Read { tag, .. } => read_tag = Some(tag),
                TMessage::Stat { tag, .. } => stat_tag = Some(tag),
                other => return Err(Error::from(format!("unexpected request: {other:?}"))),
            }
        }

        write_response(
            &mut stream,
            &RMessage::Stat {
                tag: stat_tag.ok_or("missing Tstat")?,
                stat: Stat::new(".", Qid::dir(1), crate::qid::DMDIR | 0o500),
            },
        )?;
        write_response(
            &mut stream,
            &RMessage::Read {
                tag: read_tag.ok_or("missing Tread")?,
                data: b"read after stat\n".to_vec(),
            },
        )?;
        Ok(())
    }

    #[cfg(unix)]
    fn scripted_flush_server(mut stream: UnixStream, done: mpsc::Receiver<()>) -> Result<()> {
        handshake(&mut stream)?;
        let read = read_tmessage(&mut stream)?;
        let read_tag = match read {
            TMessage::Read { tag, .. } => tag,
            other => return Err(Error::from(format!("expected Tread, got {other:?}"))),
        };
        let flush = read_tmessage(&mut stream)?;
        let flush_tag = match flush {
            TMessage::Flush { tag, oldtag } if oldtag == read_tag => tag,
            other => return Err(Error::from(format!("expected Tflush, got {other:?}"))),
        };
        write_response(&mut stream, &RMessage::Flush { tag: flush_tag })?;
        done.recv()
            .map_err(|_| Error::from("flush test ended before server release"))?;
        Ok(())
    }

    #[cfg(unix)]
    fn handshake(stream: &mut UnixStream) -> Result<()> {
        let version = read_tmessage(stream)?;
        match version {
            TMessage::Version { tag, msize, .. } => write_response(
                stream,
                &RMessage::Version {
                    tag,
                    msize,
                    version: b"9P2000".to_vec(),
                },
            )?,
            other => return Err(Error::from(format!("expected Tversion, got {other:?}"))),
        }
        let attach = read_tmessage(stream)?;
        match attach {
            TMessage::Attach { tag, fid, afid, .. } if fid != NOFID && afid == NOFID => {
                write_response(
                    &mut *stream,
                    &RMessage::Attach {
                        tag,
                        qid: Qid::dir(1),
                    },
                )?
            }
            other => return Err(Error::from(format!("expected Tattach, got {other:?}"))),
        }
        Ok(())
    }

    #[cfg(unix)]
    fn read_tmessage(stream: &mut UnixStream) -> Result<TMessage> {
        let mut prefix = [0_u8; 4];
        stream
            .read_exact(&mut prefix)
            .map_err(|error| io_error("read T-message size", error))?;
        let size = u32::from_le_bytes(prefix);
        let rest_len = usize::try_from(size - 4).map_err(|_| Error::from("oversized 9P frame"))?;
        let mut frame = Vec::with_capacity(usize::try_from(size).unwrap_or(rest_len + 4));
        frame.extend(prefix);
        frame.resize(rest_len + 4, 0);
        stream
            .read_exact(&mut frame[4..])
            .map_err(|error| io_error("read T-message body", error))?;
        codec::decode_tmessage(&frame)
            .map_err(|error| Error::from(format!("decode 9P T-message: {error}")))
    }

    #[test]
    fn pending_wait_reports_closed_reader() {
        let (sender, receiver) = mpsc::channel();
        drop(sender);
        let pending = PendingCall { tag: 7, receiver };
        let error = pending.wait().expect_err("closed reader should fail");
        assert_eq!(error.message(), b"9P reader stopped before response");
    }
}
