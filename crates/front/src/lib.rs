pub mod abi;
pub mod serve;

use r9p::error::{Error, Result};
use r9p::fid::Fid;
use r9p::qid::{Qid, QTDIR, QTFILE};
use r9p::server::{FileTree, OpenFile, ReadData};
use r9p::stat::Stat;
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const EBADFID: &str = "unknown fid";
const ENOENT: &str = "file does not exist";
const EPERM: &str = "permission denied";
const ENOTDIR: &str = "not a directory";

const ROOT_ID: u64 = 0;
const OREAD: u8 = 0;
const OWRITE: u8 = 1;
const OTRUNC: u8 = 0x10;
const ORCLOSE: u8 = 0x40;
const OPEN_MODE_MASK: u8 = 0x03;
const KNOWN_OPEN_BITS: u8 = OPEN_MODE_MASK | OTRUNC | ORCLOSE;

pub const DEFAULT_LOG_CAPACITY: usize = 1 << 20;

enum Body {
    Dir(BTreeMap<Vec<u8>, u64>),
    File(Vec<u8>),
    Log(LogBody),
    IntakeNew(u64),
}

struct LogBody {
    entries: VecDeque<Vec<u8>>,
    start: u64,
    retained: usize,
}

impl LogBody {
    fn new(bytes: Vec<u8>) -> Self {
        let retained = bytes.len();
        let mut entries = VecDeque::new();
        entries.push_back(bytes);
        Self {
            entries,
            start: 0,
            retained,
        }
    }

    fn end(&self) -> u64 {
        self.start + self.retained as u64
    }

    fn append(&mut self, bytes: Vec<u8>, capacity: usize) {
        self.retained += bytes.len();
        self.entries.push_back(bytes);
        while self.retained > capacity && self.entries.len() > 1 {
            if let Some(oldest) = self.entries.pop_front() {
                self.start += oldest.len() as u64;
                self.retained -= oldest.len();
            }
        }
    }

    fn read(&self, offset: u64, count: usize) -> Vec<u8> {
        let mut skip = usize::try_from(offset.saturating_sub(self.start)).unwrap_or(usize::MAX);
        let mut out = Vec::new();
        for entry in &self.entries {
            if skip >= entry.len() {
                skip -= entry.len();
                continue;
            }
            let take = (entry.len() - skip).min(count - out.len());
            out.extend_from_slice(&entry[skip..skip + take]);
            skip = 0;
            if out.len() == count {
                break;
            }
        }
        out
    }
}

pub struct IntakeRequest {
    pub request_id: u64,
    pub bytes: Vec<u8>,
}

struct Node {
    name: Vec<u8>,
    parent: u64,
    version: u32,
    body: Body,
}

struct State {
    nodes: BTreeMap<u64, Node>,
    next_id: u64,
    next_request_id: u64,
    intakes: BTreeMap<u64, Intake>,
    pending: VecDeque<IntakeRequest>,
    wait_timeout: Duration,
    log_capacity: usize,
}

struct Intake {
    prefix: String,
}

impl State {
    fn new() -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert(
            ROOT_ID,
            Node {
                name: b"/".to_vec(),
                parent: ROOT_ID,
                version: 0,
                body: Body::Dir(BTreeMap::new()),
            },
        );
        Self {
            nodes,
            next_id: 1,
            next_request_id: 1,
            intakes: BTreeMap::new(),
            pending: VecDeque::new(),
            wait_timeout: Duration::from_secs(30),
            log_capacity: DEFAULT_LOG_CAPACITY,
        }
    }

    fn node(&self, id: u64) -> Result<&Node> {
        self.nodes
            .get(&id)
            .ok_or_else(|| Error::from_static(ENOENT))
    }

    fn qid_for(&self, id: u64) -> Result<Qid> {
        let node = self.node(id)?;
        let qtype = match node.body {
            Body::Dir(_) => QTDIR,
            _ => QTFILE,
        };
        Ok(Qid::new(qtype, node.version, id))
    }

    fn stat_for(&self, id: u64) -> Result<Stat> {
        let node = self.node(id)?;
        let qid = self.qid_for(id)?;
        let (mode, length) = match &node.body {
            Body::Dir(_) => (0o040555u32, 0u64),
            Body::File(bytes) => (0o444u32, bytes.len() as u64),
            Body::Log(log) => (0o444u32, log.end()),
            Body::IntakeNew(_) => (0o222u32, 0u64),
        };
        Ok(Stat {
            type_: 0,
            dev: 0,
            qid,
            mode,
            atime: 0,
            mtime: node.version,
            length,
            name: if id == ROOT_ID {
                b".".to_vec()
            } else {
                node.name.clone()
            },
            uid: b"front".to_vec(),
            gid: b"front".to_vec(),
            muid: b"front".to_vec(),
        })
    }

    fn ensure_dir(&mut self, parent: u64, name: &[u8]) -> Result<u64> {
        if let Body::Dir(children) = &self.node(parent)?.body {
            if let Some(&existing) = children.get(name) {
                return match self.node(existing)?.body {
                    Body::Dir(_) => Ok(existing),
                    _ => Err(Error::from_static(ENOTDIR)),
                };
            }
        } else {
            return Err(Error::from_static(ENOTDIR));
        }
        let id = self.next_id;
        self.next_id += 1;
        self.nodes.insert(
            id,
            Node {
                name: name.to_vec(),
                parent,
                version: 0,
                body: Body::Dir(BTreeMap::new()),
            },
        );
        if let Some(Node {
            body: Body::Dir(children),
            ..
        }) = self.nodes.get_mut(&parent)
        {
            children.insert(name.to_vec(), id);
        }
        Ok(id)
    }

    fn place(&mut self, path: &str, body: Body) -> Result<u64> {
        let capacity = self.log_capacity;
        let segments = split_path(path)?;
        let (last, dirs) = segments
            .split_last()
            .ok_or_else(|| Error::from_static(EPERM))?;
        let mut parent = ROOT_ID;
        for dir in dirs {
            parent = self.ensure_dir(parent, dir)?;
        }
        let existing = match &self.node(parent)?.body {
            Body::Dir(children) => children.get(last.as_slice()).copied(),
            _ => return Err(Error::from_static(ENOTDIR)),
        };
        match existing {
            Some(id) => match self.nodes.get_mut(&id) {
                Some(node) => match (&mut node.body, body) {
                    (Body::File(_), Body::File(bytes)) => {
                        node.version = node.version.wrapping_add(1);
                        node.body = Body::File(bytes);
                        Ok(id)
                    }
                    (Body::Log(existing), Body::Log(incoming)) => {
                        node.version = node.version.wrapping_add(1);
                        for entry in incoming.entries {
                            existing.append(entry, capacity);
                        }
                        Ok(id)
                    }
                    _ => Err(Error::from_static(EPERM)),
                },
                None => Err(Error::from_static(ENOENT)),
            },
            None => {
                let id = self.next_id;
                self.next_id += 1;
                self.nodes.insert(
                    id,
                    Node {
                        name: last.clone(),
                        parent,
                        version: 0,
                        body,
                    },
                );
                if let Some(Node {
                    body: Body::Dir(children),
                    ..
                }) = self.nodes.get_mut(&parent)
                {
                    children.insert(last.clone(), id);
                }
                Ok(id)
            }
        }
    }
}

fn split_path(path: &str) -> Result<Vec<Vec<u8>>> {
    let trimmed = path.trim_matches('/');
    if trimmed.is_empty() {
        return Err(Error::from_static(EPERM));
    }
    Ok(trimmed
        .split('/')
        .map(|segment| segment.as_bytes().to_vec())
        .collect())
}

fn open_allowed(body: &Body, mode: u8) -> bool {
    if mode & !KNOWN_OPEN_BITS != 0 || mode & (OTRUNC | ORCLOSE) != 0 {
        return false;
    }
    match body {
        Body::Dir(_) | Body::File(_) | Body::Log(_) => mode & OPEN_MODE_MASK == OREAD,
        Body::IntakeNew(_) => mode & OPEN_MODE_MASK == OWRITE,
    }
}

#[derive(Clone)]
pub struct Front {
    shared: Arc<(Mutex<State>, Condvar)>,
}

impl Default for Front {
    fn default() -> Self {
        Self::new()
    }
}

impl Front {
    pub fn new() -> Self {
        Self {
            shared: Arc::new((Mutex::new(State::new()), Condvar::new())),
        }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, State>> {
        self.shared
            .0
            .lock()
            .map_err(|_| Error::from_static("front state poisoned"))
    }

    pub fn set(&self, path: &str, bytes: &[u8]) -> Result<()> {
        self.lock()?.place(path, Body::File(bytes.to_vec()))?;
        self.shared.1.notify_all();
        Ok(())
    }

    pub fn append_event(&self, path: &str, bytes: &[u8]) -> Result<()> {
        self.lock()?
            .place(path, Body::Log(LogBody::new(bytes.to_vec())))?;
        self.shared.1.notify_all();
        Ok(())
    }

    pub fn set_log_capacity(&self, capacity: usize) -> Result<()> {
        self.lock()?.log_capacity = capacity.max(1);
        Ok(())
    }

    pub fn set_wait_timeout(&self, timeout: Duration) -> Result<()> {
        self.lock()?.wait_timeout = timeout;
        Ok(())
    }

    pub fn register_intake(&self, prefix: &str) -> Result<()> {
        let mut state = self.lock()?;
        let trimmed = prefix.trim_matches('/').to_string();
        if trimmed.is_empty() {
            return Err(Error::from_static(EPERM));
        }
        let new_path = format!("{trimmed}/new");
        let id = state.place(&new_path, Body::IntakeNew(0))?;
        if let Some(node) = state.nodes.get_mut(&id) {
            node.body = Body::IntakeNew(id);
        }
        state.intakes.insert(id, Intake { prefix: trimmed });
        Ok(())
    }

    pub fn next_request(&self, timeout: Duration) -> Result<Option<IntakeRequest>> {
        let deadline = Instant::now() + timeout;
        let mut state = self.lock()?;
        loop {
            if let Some(request) = state.pending.pop_front() {
                return Ok(Some(request));
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            let (next, _timeout_result) = self
                .shared
                .1
                .wait_timeout(state, deadline - now)
                .map_err(|_| Error::from_static("front state poisoned"))?;
            state = next;
        }
    }

    pub fn complete_request(&self, prefix: &str, request_id: u64, bytes: &[u8]) -> Result<()> {
        let trimmed = prefix.trim_matches('/');
        let result_path = format!("{trimmed}/{request_id}/result");
        self.set(&result_path, bytes)
    }

    fn read_log(
        &self,
        mut state: std::sync::MutexGuard<'_, State>,
        id: u64,
        offset: u64,
        count: u32,
        cancel: Option<&AtomicBool>,
    ) -> Result<ReadData> {
        let deadline = Instant::now() + state.wait_timeout;
        loop {
            if cancel.is_some_and(|cancel| cancel.load(Ordering::SeqCst)) {
                return Err(Error::from_static("request flushed"));
            }
            if let Body::Log(log) = &state.node(id)?.body {
                if offset < log.start {
                    return Err(Error::from(format!(
                        "log window passed: earliest retained offset {}",
                        log.start
                    )));
                }
                if offset < log.end() {
                    return Ok(ReadData::Bytes(log.read(offset, count as usize)));
                }
            } else {
                return Err(Error::from_static(ENOENT));
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(ReadData::Bytes(Vec::new()));
            }
            let (next, _timeout_result) = self
                .shared
                .1
                .wait_timeout(state, deadline - now)
                .map_err(|_| Error::from_static("front state poisoned"))?;
            state = next;
        }
    }

    pub fn tree(&self) -> FrontTree {
        FrontTree {
            front: self.clone(),
            fids: BTreeMap::new(),
        }
    }

    pub(crate) fn wake_readers(&self) {
        self.shared.1.notify_all();
    }

    pub(crate) fn read_node(
        &self,
        id: u64,
        offset: u64,
        count: u32,
        cancel: Option<&AtomicBool>,
    ) -> Result<ReadData> {
        let state = self.lock()?;
        match &state.node(id)?.body {
            Body::Dir(children) => {
                let mut stats = Vec::with_capacity(children.len());
                for &child in children.values() {
                    stats.push(state.stat_for(child)?);
                }
                Ok(ReadData::Directory(stats))
            }
            Body::File(bytes) => {
                let start = usize::try_from(offset.min(bytes.len() as u64))
                    .map_err(|_| Error::from_static(EPERM))?;
                let end = bytes.len().min(start.saturating_add(count as usize));
                Ok(ReadData::Bytes(bytes[start..end].to_vec()))
            }
            Body::Log(_) => self.read_log(state, id, offset, count, cancel),
            Body::IntakeNew(_) => Err(Error::from_static(EPERM)),
        }
    }
}

pub struct FrontTree {
    front: Front,
    fids: BTreeMap<Fid, u64>,
}

impl FileTree for FrontTree {
    fn attach(&mut self, fid: Fid, _uname: &[u8], _aname: &[u8]) -> Result<Qid> {
        let state = self.front.lock()?;
        let qid = state.qid_for(ROOT_ID)?;
        self.fids.insert(fid, ROOT_ID);
        Ok(qid)
    }

    fn walk(&mut self, fid: Fid, newfid: Fid, _start: Qid, names: &[Vec<u8>]) -> Result<Vec<Qid>> {
        let state = self.front.lock()?;
        let mut current = *self
            .fids
            .get(&fid)
            .ok_or_else(|| Error::from_static(EBADFID))?;
        let mut qids = Vec::with_capacity(names.len());
        for name in names {
            let child = if name.as_slice() == b".." {
                Some(state.node(current)?.parent)
            } else {
                match &state.node(current)?.body {
                    Body::Dir(children) => children.get(name.as_slice()).copied(),
                    _ => None,
                }
            };
            match child {
                Some(id) => {
                    qids.push(state.qid_for(id)?);
                    current = id;
                }
                None => break,
            }
        }
        if qids.len() == names.len() {
            self.fids.insert(newfid, current);
        }
        Ok(qids)
    }

    fn open(&mut self, fid: Fid, qid: Qid, mode: u8) -> Result<OpenFile> {
        let state = self.front.lock()?;
        let id = *self
            .fids
            .get(&fid)
            .ok_or_else(|| Error::from_static(EBADFID))?;
        if state.qid_for(id)?.path != qid.path {
            return Err(Error::from_static(EBADFID));
        }
        if !open_allowed(&state.node(id)?.body, mode) {
            return Err(Error::from_static(EPERM));
        }
        Ok(OpenFile { qid, iounit: 0 })
    }

    fn read(&mut self, fid: Fid, _qid: Qid, offset: u64, count: u32) -> Result<ReadData> {
        let id = self.read_target(fid)?;
        self.front.read_node(id, offset, count, None)
    }

    fn write(&mut self, fid: Fid, _qid: Qid, _offset: u64, data: &[u8]) -> Result<u32> {
        let mut state = self.front.lock()?;
        let id = *self
            .fids
            .get(&fid)
            .ok_or_else(|| Error::from_static(EBADFID))?;
        let intake_id = match state.node(id)?.body {
            Body::IntakeNew(intake_id) => intake_id,
            _ => return Err(Error::from_static(EPERM)),
        };
        let prefix = state
            .intakes
            .get(&intake_id)
            .ok_or_else(|| Error::from_static(ENOENT))?
            .prefix
            .clone();
        let request_id = state.next_request_id;
        state.next_request_id = state.next_request_id.saturating_add(1);
        state.place(
            &format!("{prefix}/{request_id}/request"),
            Body::File(data.to_vec()),
        )?;
        state.place(
            &format!("{prefix}/created"),
            Body::File(request_id.to_string().into_bytes()),
        )?;
        state.pending.push_back(IntakeRequest {
            request_id,
            bytes: data.to_vec(),
        });
        drop(state);
        self.front.shared.1.notify_all();
        u32::try_from(data.len()).map_err(|_| Error::from_static(EPERM))
    }

    fn stat(&mut self, qid: Qid) -> Result<Stat> {
        self.front.lock()?.stat_for(qid.path)
    }

    fn clunk(&mut self, fid: Fid, _qid: Qid) -> Result<()> {
        self.fids.remove(&fid);
        Ok(())
    }
}

impl FrontTree {
    pub(crate) fn read_target(&self, fid: Fid) -> Result<u64> {
        self.fids
            .get(&fid)
            .copied()
            .ok_or_else(|| Error::from_static(EBADFID))
    }

    pub(crate) fn front(&self) -> Front {
        self.front.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk_to(tree: &mut FrontTree, fid: Fid, newfid: Fid, path: &[&str]) -> Vec<Qid> {
        let names: Vec<Vec<u8>> = path.iter().map(|name| name.as_bytes().to_vec()).collect();
        let start = Qid::new(QTDIR, 0, ROOT_ID);
        tree.walk(fid, newfid, start, &names)
            .expect("walk should succeed")
    }

    #[test]
    fn set_then_walk_and_read_roundtrip() -> Result<()> {
        let front = Front::new();
        front.set("market/status", b"#M(\"state\" 'open)")?;
        let mut tree = front.tree();
        tree.attach(1, b"claude", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["market", "status"]);
        assert_eq!(qids.len(), 2);
        let open = tree.open(2, qids[1], 0)?;
        let data = tree.read(2, open.qid, 0, 4096)?;
        assert_eq!(data, ReadData::Bytes(b"#M(\"state\" 'open)".to_vec()));
        Ok(())
    }

    #[test]
    fn overwrite_bumps_version_and_serves_new_bytes() -> Result<()> {
        let front = Front::new();
        front.set("market/status", b"first")?;
        front.set("market/status", b"second")?;
        let mut tree = front.tree();
        tree.attach(1, b"claude", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["market", "status"]);
        assert_eq!(qids[1].version, 1);
        let stat = tree.stat(qids[1])?;
        assert_eq!(stat.length, 6);
        let data = tree.read(2, qids[1], 0, 4096)?;
        assert_eq!(data, ReadData::Bytes(b"second".to_vec()));
        Ok(())
    }

    #[test]
    fn missing_path_walks_partially() -> Result<()> {
        let front = Front::new();
        front.set("market/status", b"x")?;
        let mut tree = front.tree();
        tree.attach(1, b"claude", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["market", "absent"]);
        assert_eq!(qids.len(), 1);
        Ok(())
    }

    #[test]
    fn log_appends_and_reads_in_order() -> Result<()> {
        let front = Front::new();
        front.append_event("market/events", b"one\n")?;
        front.append_event("market/events", b"two\n")?;
        let mut tree = front.tree();
        tree.attach(1, b"claude", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["market", "events"]);
        let data = tree.read(2, qids[1], 0, 4096)?;
        assert_eq!(data, ReadData::Bytes(b"one\ntwo\n".to_vec()));
        let tail = tree.read(2, qids[1], 4, 4096)?;
        assert_eq!(tail, ReadData::Bytes(b"two\n".to_vec()));
        Ok(())
    }

    #[test]
    fn log_window_drops_whole_entries_and_keeps_absolute_offsets() -> Result<()> {
        let front = Front::new();
        front.set_log_capacity(10)?;
        front.append_event("market/events", b"aaaa\n")?;
        front.append_event("market/events", b"bbbb\n")?;
        front.append_event("market/events", b"cccc\n")?;
        let mut tree = front.tree();
        tree.attach(1, b"claude", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["market", "events"]);
        let stat = tree.stat(qids[1])?;
        assert_eq!(stat.length, 15);
        let data = tree.read(2, qids[1], 5, 4096)?;
        assert_eq!(data, ReadData::Bytes(b"bbbb\ncccc\n".to_vec()));
        let mid = tree.read(2, qids[1], 12, 4096)?;
        assert_eq!(mid, ReadData::Bytes(b"cc\n".to_vec()));
        Ok(())
    }

    #[test]
    fn log_read_behind_window_fails_typed_with_earliest_offset() -> Result<()> {
        let front = Front::new();
        front.set_log_capacity(10)?;
        front.append_event("market/events", b"aaaa\n")?;
        front.append_event("market/events", b"bbbb\n")?;
        front.append_event("market/events", b"cccc\n")?;
        let mut tree = front.tree();
        tree.attach(1, b"claude", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["market", "events"]);
        let error = tree
            .read(2, qids[1], 0, 4096)
            .expect_err("behind-window read must fail");
        assert_eq!(
            error.message(),
            b"log window passed: earliest retained offset 5"
        );
        Ok(())
    }

    #[test]
    fn log_keeps_a_single_oversized_entry() -> Result<()> {
        let front = Front::new();
        front.set_log_capacity(4)?;
        front.append_event("market/events", b"0123456789\n")?;
        let mut tree = front.tree();
        tree.attach(1, b"claude", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["market", "events"]);
        let data = tree.read(2, qids[1], 0, 4096)?;
        assert_eq!(data, ReadData::Bytes(b"0123456789\n".to_vec()));
        Ok(())
    }

    #[test]
    fn log_read_at_tail_blocks_until_push() -> Result<()> {
        let front = Front::new();
        front.append_event("market/events", b"seed\n")?;
        front.set_wait_timeout(Duration::from_secs(5))?;
        let pusher = front.clone();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(80));
            pusher.append_event("market/events", b"wake\n")
        });
        let mut tree = front.tree();
        tree.attach(1, b"claude", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["market", "events"]);
        let started = Instant::now();
        let data = tree.read(2, qids[1], 5, 4096)?;
        assert_eq!(data, ReadData::Bytes(b"wake\n".to_vec()));
        assert!(started.elapsed() >= Duration::from_millis(50));
        handle.join().expect("push thread").expect("push result");
        Ok(())
    }

    #[test]
    fn log_read_at_tail_times_out_empty() -> Result<()> {
        let front = Front::new();
        front.append_event("market/events", b"seed\n")?;
        front.set_wait_timeout(Duration::from_millis(60))?;
        let mut tree = front.tree();
        tree.attach(1, b"claude", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["market", "events"]);
        let data = tree.read(2, qids[1], 5, 4096)?;
        assert_eq!(data, ReadData::Bytes(Vec::new()));
        Ok(())
    }

    #[test]
    fn intake_write_lands_request_and_completion() -> Result<()> {
        let front = Front::new();
        front.register_intake("queries")?;
        let mut tree = front.tree();
        tree.attach(1, b"claude", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["queries", "new"]);
        tree.open(2, qids[1], 1)?;
        let wrote = tree.write(2, qids[1], 0, b"#M(\"kind\" \"search\")")?;
        assert_eq!(wrote as usize, b"#M(\"kind\" \"search\")".len());
        let request = front
            .next_request(Duration::from_millis(200))?
            .expect("pending request");
        assert_eq!(request.request_id, 1);
        assert_eq!(request.bytes, b"#M(\"kind\" \"search\")".to_vec());
        front.complete_request("queries", request.request_id, b"#M(\"hits\" ())")?;
        let qids = walk_to(&mut tree, 1, 3, &["queries", "1", "result"]);
        let data = tree.read(3, qids[2], 0, 4096)?;
        assert_eq!(data, ReadData::Bytes(b"#M(\"hits\" ())".to_vec()));
        let created = walk_to(&mut tree, 1, 4, &["queries", "created"]);
        let marker = tree.read(4, created[1], 0, 64)?;
        assert_eq!(marker, ReadData::Bytes(b"1".to_vec()));
        Ok(())
    }

    #[test]
    fn intake_new_rejects_reads_and_plain_files_reject_writes() -> Result<()> {
        let front = Front::new();
        front.register_intake("queries")?;
        front.set("market/status", b"x")?;
        let mut tree = front.tree();
        tree.attach(1, b"claude", b"/")?;
        let new_qids = walk_to(&mut tree, 1, 2, &["queries", "new"]);
        assert!(tree.open(2, new_qids[1], 0).is_err());
        let file_qids = walk_to(&mut tree, 1, 3, &["market", "status"]);
        assert!(tree.open(3, file_qids[1], 1).is_err());
        assert!(tree.write(3, file_qids[1], 0, b"nope").is_err());
        Ok(())
    }

    #[test]
    fn open_modes_are_exact_permissions_not_writeish_bits() -> Result<()> {
        let front = Front::new();
        front.register_intake("queries")?;
        front.set("market/status", b"x")?;
        let mut tree = front.tree();
        tree.attach(1, b"claude", b"/")?;
        let new_qids = walk_to(&mut tree, 1, 2, &["queries", "new"]);
        assert!(tree.open(2, new_qids[1], OWRITE).is_ok());
        assert!(tree.open(2, new_qids[1], 2).is_err());
        assert!(tree.open(2, new_qids[1], 3).is_err());
        assert!(tree.open(2, new_qids[1], OWRITE | OTRUNC).is_err());
        let file_qids = walk_to(&mut tree, 1, 3, &["market", "status"]);
        assert!(tree.open(3, file_qids[1], OREAD).is_ok());
        assert!(tree.open(3, file_qids[1], 3).is_err());
        assert!(tree.open(3, file_qids[1], OREAD | ORCLOSE).is_err());
        Ok(())
    }

    #[test]
    fn tree_fids_are_per_connection() -> Result<()> {
        let front = Front::new();
        front.set("market/status", b"x")?;
        let mut first = front.tree();
        let mut second = front.tree();
        first.attach(1, b"first", b"/")?;
        second.attach(1, b"second", b"/")?;
        let first_qids = walk_to(&mut first, 1, 2, &["market", "status"]);
        first.clunk(1, Qid::dir(ROOT_ID))?;
        let second_qids = walk_to(&mut second, 1, 2, &["market", "status"]);
        assert_eq!(first_qids.len(), 2);
        assert_eq!(second_qids.len(), 2);
        Ok(())
    }

    #[test]
    fn dotdot_walks_to_parent() -> Result<()> {
        let front = Front::new();
        front.set("market/status", b"x")?;
        let mut tree = front.tree();
        tree.attach(1, b"claude", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["market", "status"]);
        assert_eq!(qids.len(), 2);
        let names = vec![b"..".to_vec(), b"..".to_vec()];
        let back = tree.walk(2, 3, qids[1], &names).expect("dotdot walk");
        assert_eq!(back.len(), 2);
        assert_eq!(back[1].path, ROOT_ID);
        Ok(())
    }

    #[test]
    fn directory_read_lists_children() -> Result<()> {
        let front = Front::new();
        front.set("market/status", b"x")?;
        front.set("market/events", b"y")?;
        let mut tree = front.tree();
        tree.attach(1, b"claude", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["market"]);
        let data = tree.read(2, qids[0], 0, 4096)?;
        match data {
            ReadData::Directory(stats) => {
                assert_eq!(stats.len(), 2);
            }
            ReadData::Bytes(_) => panic!("expected directory listing"),
        }
        Ok(())
    }
}
