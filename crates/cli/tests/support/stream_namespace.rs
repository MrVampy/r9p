use std::{
    collections::{BTreeMap, VecDeque},
    io,
    io::Write,
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use r9p::{
    codec,
    fid::Fid,
    qid::{Qid, DMDIR, QTFILE},
    server::{FileTree, OpenFile, ReadData, Server},
    stat::Stat,
    Error as R9pError, Result as R9pResult,
};

const ROOT: Qid = Qid::dir(1);
const WATCHED: Qid = Qid::file(2);
const MUTATE: Qid = Qid::file(3);
const EVENTS: Qid = Qid::dir(4);
const NAMESPACE: Qid = Qid::dir(5);
const RECENT: Qid = Qid::file(6);
const STREAM: Qid = Qid::file(7);
const CREATED_PATH: u64 = 8;

pub struct NamespaceServer {
    pub endpoint: String,
    state: SharedNamespace,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<io::Result<()>>>,
}

impl NamespaceServer {
    pub fn start() -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let endpoint = listener.local_addr()?.to_string();
        let state = SharedNamespace::new();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_state = state.clone();
        let thread_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || serve_connections(listener, thread_state, thread_stop));
        Ok(Self {
            endpoint,
            state,
            stop,
            handle: Some(handle),
        })
    }

    pub fn wait_for_stream_reader_count(&self, expected: usize) -> io::Result<()> {
        let started = Instant::now();
        while started.elapsed() <= Duration::from_secs(5) {
            if self.state.stream_reader_count() == expected {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(20));
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("stream reader count did not reach {expected}"),
        ))
    }
}

impl Drop for NamespaceServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.state.stop();
        let _ = TcpStream::connect(&self.endpoint);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[derive(Clone)]
struct SharedNamespace {
    inner: Arc<(Mutex<NamespaceState>, Condvar)>,
    next_reader: Arc<AtomicU64>,
}

struct NamespaceState {
    created: bool,
    generation: u64,
    recent: Vec<String>,
    stream_queues: BTreeMap<u64, VecDeque<u8>>,
    stopped: bool,
}

impl SharedNamespace {
    fn new() -> Self {
        Self {
            inner: Arc::new((
                Mutex::new(NamespaceState {
                    created: false,
                    generation: 1,
                    recent: Vec::new(),
                    stream_queues: BTreeMap::new(),
                    stopped: false,
                }),
                Condvar::new(),
            )),
            next_reader: Arc::new(AtomicU64::new(1)),
        }
    }

    fn created(&self) -> bool {
        let (lock, _) = &*self.inner;
        lock.lock().map(|state| state.created).unwrap_or(false)
    }

    fn generation(&self) -> u64 {
        let (lock, _) = &*self.inner;
        lock.lock().map(|state| state.generation).unwrap_or(1)
    }

    fn mutate(&self) {
        let (lock, notify) = &*self.inner;
        let mut state = lock.lock().expect("namespace state lock");
        if state.created {
            return;
        }
        state.created = true;
        state.generation = state.generation.saturating_add(1);
        let event_id = "event-1".to_string();
        let record = format!(
            "namespace_change\tevent_id={event_id}\tgeneration={}\tscope=shared\tchange_kind=created\tpath=/created.txt\n",
            state.generation
        );
        state.recent.push(record.clone());
        for queue in state.stream_queues.values_mut() {
            queue.extend(record.as_bytes());
        }
        notify.notify_all();
    }

    fn recent_records(&self) -> Vec<u8> {
        let (lock, _) = &*self.inner;
        lock.lock()
            .map(|state| state.recent.join("").into_bytes())
            .unwrap_or_default()
    }

    fn add_stream_reader(&self) -> u64 {
        let reader = self.next_reader.fetch_add(1, Ordering::SeqCst);
        let (lock, notify) = &*self.inner;
        if let Ok(mut state) = lock.lock() {
            state.stream_queues.insert(reader, VecDeque::new());
            notify.notify_all();
        }
        reader
    }

    fn remove_stream_reader(&self, reader: u64) {
        let (lock, _) = &*self.inner;
        if let Ok(mut state) = lock.lock() {
            state.stream_queues.remove(&reader);
        }
    }

    fn stream_reader_count(&self) -> usize {
        let (lock, _) = &*self.inner;
        lock.lock()
            .map(|state| state.stream_queues.len())
            .unwrap_or_default()
    }

    fn read_stream(&self, reader: u64, count: u32) -> R9pResult<Vec<u8>> {
        let (lock, notify) = &*self.inner;
        let mut state = lock
            .lock()
            .map_err(|_| R9pError::from("namespace state lock poisoned"))?;
        while !state.stopped
            && state
                .stream_queues
                .get(&reader)
                .map(|queue| queue.is_empty())
                .unwrap_or(true)
        {
            let result = notify
                .wait_timeout(state, Duration::from_millis(250))
                .map_err(|_| R9pError::from("namespace state lock poisoned"))?;
            state = result.0;
            if result.1.timed_out() {
                return Ok(Vec::new());
            }
        }
        if state.stopped {
            return Ok(Vec::new());
        }
        let queue = state
            .stream_queues
            .get_mut(&reader)
            .ok_or_else(|| R9pError::from("stream reader missing"))?;
        let take = usize::try_from(count)
            .unwrap_or(usize::MAX)
            .min(queue.len());
        Ok(queue.drain(..take).collect())
    }

    fn stop(&self) {
        let (lock, notify) = &*self.inner;
        if let Ok(mut state) = lock.lock() {
            state.stopped = true;
            notify.notify_all();
        }
    }
}

struct NamespaceTree {
    state: SharedNamespace,
    stream_readers: BTreeMap<Fid, u64>,
}

impl NamespaceTree {
    fn new(state: SharedNamespace) -> Self {
        Self {
            state,
            stream_readers: BTreeMap::new(),
        }
    }

    fn created_qid(&self) -> Qid {
        Qid::new(QTFILE, self.state.generation() as u32, CREATED_PATH)
    }
}

impl FileTree for NamespaceTree {
    fn attach(&mut self, _fid: Fid, _uname: &[u8], _aname: &[u8]) -> R9pResult<Qid> {
        Ok(ROOT)
    }

    fn walk(
        &mut self,
        _fid: Fid,
        _newfid: Fid,
        start: Qid,
        names: &[Vec<u8>],
    ) -> R9pResult<Vec<Qid>> {
        match names {
            [] => Ok(Vec::new()),
            [name] if start == ROOT && name == b"watched.txt" => Ok(vec![WATCHED]),
            [name] if start == ROOT && name == b"mutate" => Ok(vec![MUTATE]),
            [name] if start == ROOT && name == b"created.txt" && self.state.created() => {
                Ok(vec![self.created_qid()])
            }
            [name] if start == ROOT && name == b"events" => Ok(vec![EVENTS]),
            [parent, child] if start == ROOT && parent == b"events" && child == b"namespace" => {
                Ok(vec![EVENTS, NAMESPACE])
            }
            [grandparent, parent, child]
                if start == ROOT
                    && grandparent == b"events"
                    && parent == b"namespace"
                    && child == b"recent" =>
            {
                Ok(vec![EVENTS, NAMESPACE, RECENT])
            }
            [grandparent, parent, child]
                if start == ROOT
                    && grandparent == b"events"
                    && parent == b"namespace"
                    && child == b"stream" =>
            {
                Ok(vec![EVENTS, NAMESPACE, STREAM])
            }
            [name] if start == EVENTS && name == b"namespace" => Ok(vec![NAMESPACE]),
            [name] if start == NAMESPACE && name == b"recent" => Ok(vec![RECENT]),
            [name] if start == NAMESPACE && name == b"stream" => Ok(vec![STREAM]),
            _ => Ok(Vec::new()),
        }
    }

    fn open(&mut self, fid: Fid, qid: Qid, _mode: u8) -> R9pResult<OpenFile> {
        if qid == STREAM {
            let reader = self.state.add_stream_reader();
            self.stream_readers.insert(fid, reader);
        }
        Ok(OpenFile { qid, iounit: 0 })
    }

    fn read(&mut self, fid: Fid, qid: Qid, offset: u64, count: u32) -> R9pResult<ReadData> {
        if qid == ROOT {
            let mut entries = vec![
                stat_for("watched.txt", WATCHED, b"watched\n".len() as u64),
                stat_for("mutate", MUTATE, 0),
                Stat::new("events", EVENTS, DMDIR | 0o555),
            ];
            if self.state.created() {
                entries.push(stat_for(
                    "created.txt",
                    self.created_qid(),
                    b"created\n".len() as u64,
                ));
            }
            return Ok(ReadData::Directory(entries));
        }
        if qid == EVENTS {
            return Ok(ReadData::Directory(vec![Stat::new(
                "namespace",
                NAMESPACE,
                DMDIR | 0o555,
            )]));
        }
        if qid == NAMESPACE {
            return Ok(ReadData::Directory(vec![
                stat_for("recent", RECENT, self.state.recent_records().len() as u64),
                stat_for("stream", STREAM, 0),
            ]));
        }
        if qid == RECENT {
            return slice_bytes(&self.state.recent_records(), offset, count);
        }
        if qid == STREAM {
            let reader = self
                .stream_readers
                .get(&fid)
                .copied()
                .ok_or_else(|| R9pError::from("stream reader missing"))?;
            return self.state.read_stream(reader, count).map(ReadData::Bytes);
        }
        if qid == WATCHED {
            return slice_bytes(b"watched\n", offset, count);
        }
        if qid.path == CREATED_PATH && self.state.created() {
            return slice_bytes(b"created\n", offset, count);
        }
        slice_bytes(b"", offset, count)
    }

    fn stat(&mut self, qid: Qid) -> R9pResult<Stat> {
        match qid {
            ROOT => Ok(Stat::new("", ROOT, DMDIR | 0o555)),
            WATCHED => Ok(stat_for("watched.txt", WATCHED, b"watched\n".len() as u64)),
            MUTATE => Ok(stat_for("mutate", MUTATE, 0)),
            EVENTS => Ok(Stat::new("events", EVENTS, DMDIR | 0o555)),
            NAMESPACE => Ok(Stat::new("namespace", NAMESPACE, DMDIR | 0o555)),
            RECENT => Ok(stat_for(
                "recent",
                RECENT,
                self.state.recent_records().len() as u64,
            )),
            STREAM => Ok(stat_for("stream", STREAM, 0)),
            qid if qid.path == CREATED_PATH && self.state.created() => Ok(stat_for(
                "created.txt",
                self.created_qid(),
                b"created\n".len() as u64,
            )),
            _ => Err(R9pError::from("file does not exist")),
        }
    }

    fn write(&mut self, _fid: Fid, qid: Qid, _offset: u64, data: &[u8]) -> R9pResult<u32> {
        if qid != MUTATE {
            return Err(R9pError::from("permission denied"));
        }
        self.state.mutate();
        Ok(u32::try_from(data.len()).unwrap_or(u32::MAX))
    }

    fn clunk(&mut self, fid: Fid, qid: Qid) -> R9pResult<()> {
        if qid == STREAM {
            if let Some(reader) = self.stream_readers.remove(&fid) {
                self.state.remove_stream_reader(reader);
            }
        }
        Ok(())
    }
}

fn stat_for(name: &str, qid: Qid, length: u64) -> Stat {
    let mut stat = Stat::new(name, qid, 0o444);
    stat.length = length;
    stat
}

fn slice_bytes(data: &[u8], offset: u64, count: u32) -> R9pResult<ReadData> {
    let start = usize::try_from(offset)
        .map_err(|_| R9pError::from("read offset too large"))?
        .min(data.len());
    let end = start
        .saturating_add(usize::try_from(count).unwrap_or(usize::MAX))
        .min(data.len());
    Ok(ReadData::Bytes(data[start..end].to_vec()))
}

fn serve_connections(
    listener: TcpListener,
    state: SharedNamespace,
    stop: Arc<AtomicBool>,
) -> io::Result<()> {
    let mut handles = Vec::new();
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let connection_state = state.clone();
                handles.push(thread::spawn(move || {
                    serve_connection(stream, connection_state)
                }));
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(error),
        }
    }
    for handle in handles {
        let _ = handle.join();
    }
    Ok(())
}

fn serve_connection(mut stream: TcpStream, state: SharedNamespace) -> io::Result<()> {
    let mut server = Server::new(NamespaceTree::new(state));
    while let Some(message) = codec::read_tmessage_checked(&mut stream, server.session().msize())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?
    {
        let reply = server.handle(message);
        let frame = codec::encode_rmessage_checked(&reply, server.session().msize())
            .map_err(|error| io::Error::other(format!("encode reply: {error}")))?;
        if let Err(error) = stream.write_all(&frame) {
            if matches!(
                error.kind(),
                io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
            ) {
                return Ok(());
            }
            return Err(error);
        }
    }
    Ok(())
}
