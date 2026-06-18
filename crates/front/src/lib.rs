pub mod abi;
pub mod serve;

use r9p::codec::{MAX_MSIZE, MIN_MSIZE};
use r9p::error::{Error, Result};
use r9p::fid::Fid;
use r9p::qid::{Qid, DMDIR, QTDIR, QTFILE};
use r9p::server::{FileTree, OpenFile, ReadData};
use r9p::stat::Stat;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const EBADFID: &str = "unknown fid";
const ENOENT: &str = "file does not exist";
const EPERM: &str = "permission denied";
const ENOTDIR: &str = "not a directory";

const ROOT_ID: u64 = 0;
const OREAD: u8 = 0;
const OWRITE: u8 = 1;
const ORDWR: u8 = 2;
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
    Rpc(String),
    WriteRelay(String),
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

    fn empty() -> Self {
        Self {
            entries: VecDeque::new(),
            start: 0,
            retained: 0,
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
    pub prefix: String,
    pub bytes: Vec<u8>,
    pub context: RequestContext,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestContext {
    pub uname: String,
    pub aname: String,
    pub session_id: u64,
    pub fid: Fid,
    pub target_path: String,
    pub offset: u64,
    pub open_mode: u8,
    pub pushed_generation: u64,
}

impl RequestContext {
    fn from_parts(
        binding: &FidBinding,
        fid: Fid,
        target_path: String,
        offset: u64,
        open_mode: u8,
        pushed_generation: u64,
    ) -> Self {
        Self {
            uname: String::from_utf8_lossy(&binding.uname).into_owned(),
            aname: String::from_utf8_lossy(&binding.aname).into_owned(),
            session_id: binding.session_id,
            fid,
            target_path,
            offset,
            open_mode,
            pushed_generation,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushedFileMetadata {
    pub qid_path: u64,
    pub qid_version: u32,
    pub generation: u64,
    pub visibility_class: String,
    pub wake_token: String,
}

enum WriteRelayReply {
    Accepted(u32),
    Rejected(String),
}

struct Node {
    name: Vec<u8>,
    parent: u64,
    qid_path: u64,
    version: u32,
    generation: u64,
    visibility_class: Option<String>,
    wake_token: Option<String>,
    body: Body,
}

struct State {
    nodes: BTreeMap<u64, Node>,
    qid_index: BTreeMap<u64, u64>,
    next_id: u64,
    next_request_id: u64,
    intakes: BTreeMap<u64, Intake>,
    pending: VecDeque<IntakeRequest>,
    rpc_responses: BTreeMap<u64, Option<Vec<u8>>>,
    write_relay_responses: BTreeMap<u64, Option<WriteRelayReply>>,
    principal_roots_required: bool,
    principal_roots: BTreeMap<Vec<u8>, PrincipalRoot>,
    wait_timeout: Duration,
    log_capacity: usize,
    protocol: ProtocolConfig,
}

struct Intake {
    prefix: String,
}

struct PrincipalRoot {
    root: u64,
    anames: BTreeSet<Vec<u8>>,
}

#[derive(Clone, Copy)]
struct ProtocolConfig {
    max_msize: u32,
    iounit: u32,
}

impl Default for ProtocolConfig {
    fn default() -> Self {
        Self {
            max_msize: MAX_MSIZE,
            iounit: 0,
        }
    }
}

impl State {
    fn new() -> Self {
        let mut nodes = BTreeMap::new();
        let mut qid_index = BTreeMap::new();
        qid_index.insert(ROOT_ID, ROOT_ID);
        nodes.insert(
            ROOT_ID,
            Node {
                name: b"/".to_vec(),
                parent: ROOT_ID,
                qid_path: ROOT_ID,
                version: 0,
                generation: 0,
                visibility_class: None,
                wake_token: None,
                body: Body::Dir(BTreeMap::new()),
            },
        );
        Self {
            nodes,
            qid_index,
            next_id: 1,
            next_request_id: 1,
            intakes: BTreeMap::new(),
            pending: VecDeque::new(),
            rpc_responses: BTreeMap::new(),
            write_relay_responses: BTreeMap::new(),
            principal_roots_required: false,
            principal_roots: BTreeMap::new(),
            wait_timeout: Duration::from_secs(30),
            log_capacity: DEFAULT_LOG_CAPACITY,
            protocol: ProtocolConfig::default(),
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
        Ok(Qid::new(qtype, node.version, node.qid_path))
    }

    fn node_id_for_qid_path(&self, qid_path: u64) -> Result<u64> {
        self.qid_index
            .get(&qid_path)
            .copied()
            .ok_or_else(|| Error::from_static(ENOENT))
    }

    fn replace_qid_path(&mut self, id: u64, qid_path: u64) -> Result<()> {
        if let Some(owner) = self.qid_index.get(&qid_path) {
            if *owner != id {
                return Err(Error::from_static("qid path already in use"));
            }
        }
        let old_qid_path = self.node(id)?.qid_path;
        if old_qid_path != qid_path {
            self.qid_index.remove(&old_qid_path);
            self.qid_index.insert(qid_path, id);
            if let Some(node) = self.nodes.get_mut(&id) {
                node.qid_path = qid_path;
            }
        }
        Ok(())
    }

    fn stat_for(&self, id: u64) -> Result<Stat> {
        let node = self.node(id)?;
        let qid = self.qid_for(id)?;
        let (mode, length) = match &node.body {
            Body::Dir(_) => (DMDIR | 0o555, 0u64),
            Body::File(bytes) => (0o444u32, bytes.len() as u64),
            Body::Log(log) => (0o444u32, log.end()),
            Body::IntakeNew(_) => (0o222u32, 0u64),
            Body::Rpc(_) => (0o600u32, 0u64),
            Body::WriteRelay(_) => (0o222u32, 0u64),
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
                qid_path: id,
                version: 0,
                generation: 0,
                visibility_class: None,
                wake_token: None,
                body: Body::Dir(BTreeMap::new()),
            },
        );
        self.qid_index.insert(id, id);
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
                        qid_path: id,
                        version: 0,
                        generation: 0,
                        visibility_class: None,
                        wake_token: None,
                        body,
                    },
                );
                self.qid_index.insert(id, id);
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

    fn place_pushed_file(
        &mut self,
        path: &str,
        bytes: Vec<u8>,
        metadata: PushedFileMetadata,
    ) -> Result<u64> {
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
            Some(id) => {
                if !matches!(self.node(id)?.body, Body::File(_)) {
                    return Err(Error::from_static(EPERM));
                }
                self.replace_qid_path(id, metadata.qid_path)?;
                if let Some(node) = self.nodes.get_mut(&id) {
                    node.version = metadata.qid_version;
                    node.generation = metadata.generation;
                    node.visibility_class = Some(metadata.visibility_class);
                    node.wake_token = Some(metadata.wake_token);
                    node.body = Body::File(bytes);
                }
                Ok(id)
            }
            None => {
                if self.qid_index.contains_key(&metadata.qid_path) {
                    return Err(Error::from_static("qid path already in use"));
                }
                let id = self.next_id;
                self.next_id += 1;
                self.nodes.insert(
                    id,
                    Node {
                        name: last.clone(),
                        parent,
                        qid_path: metadata.qid_path,
                        version: metadata.qid_version,
                        generation: metadata.generation,
                        visibility_class: Some(metadata.visibility_class),
                        wake_token: Some(metadata.wake_token),
                        body: Body::File(bytes),
                    },
                );
                self.qid_index.insert(metadata.qid_path, id);
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

    fn lookup_path(&self, path: &str) -> Result<u64> {
        let trimmed = path.trim_matches('/');
        if trimmed.is_empty() {
            return Ok(ROOT_ID);
        }
        let mut current = ROOT_ID;
        for segment in split_path(trimmed)? {
            let node = self.node(current)?;
            let child = match &node.body {
                Body::Dir(children) => children.get(segment.as_slice()).copied(),
                _ => return Err(Error::from_static(ENOTDIR)),
            };
            current = child.ok_or_else(|| Error::from_static(ENOENT))?;
        }
        Ok(current)
    }

    fn is_intake_prefix(&self, prefix: &str) -> bool {
        self.intakes.values().any(|intake| intake.prefix == prefix)
    }

    fn remove_pending_request(&mut self, request_id: u64) {
        self.pending
            .retain(|request| request.request_id != request_id);
    }

    fn path_relative_to(&self, id: u64, root: u64) -> Result<String> {
        let mut current = id;
        let mut segments = Vec::new();
        loop {
            if current == root {
                break;
            }
            if current == ROOT_ID {
                return Err(Error::from_static(EPERM));
            }
            let node = self.node(current)?;
            segments.push(String::from_utf8_lossy(&node.name).into_owned());
            current = node.parent;
        }
        segments.reverse();
        if segments.is_empty() {
            Ok("/".to_string())
        } else {
            Ok(format!("/{}", segments.join("/")))
        }
    }

    fn attach_root_for(&self, uname: &[u8], aname: &[u8]) -> Result<u64> {
        if !self.principal_roots_required {
            return Ok(ROOT_ID);
        }
        let root = self
            .principal_roots
            .get(uname)
            .ok_or_else(|| Error::from_static("principal root unavailable"))?;
        if root.anames.contains(b"*".as_slice()) || root.anames.contains(aname) {
            Ok(root.root)
        } else {
            Err(Error::from_static("principal aname unavailable"))
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
        Body::IntakeNew(_) | Body::WriteRelay(_) => mode & OPEN_MODE_MASK == OWRITE,
        Body::Rpc(_) => mode & OPEN_MODE_MASK == ORDWR,
    }
}

#[derive(Clone)]
pub struct Front {
    shared: Arc<(Mutex<State>, Condvar)>,
    next_session_id: Arc<AtomicU64>,
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
            next_session_id: Arc::new(AtomicU64::new(1)),
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

    pub fn set_pushed_file(
        &self,
        path: &str,
        bytes: &[u8],
        metadata: PushedFileMetadata,
    ) -> Result<()> {
        self.lock()?
            .place_pushed_file(path, bytes.to_vec(), metadata)?;
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

    pub fn set_protocol_limits(&self, max_msize: u32, iounit: u32) -> Result<()> {
        if !(MIN_MSIZE..=MAX_MSIZE).contains(&max_msize) {
            return Err(Error::from_static("invalid max msize"));
        }
        if iounit > max_msize {
            return Err(Error::from_static("invalid iounit"));
        }
        self.lock()?.protocol = ProtocolConfig { max_msize, iounit };
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

    pub fn register_rpc(&self, path: &str) -> Result<()> {
        let mut state = self.lock()?;
        let trimmed = path.trim_matches('/');
        if trimmed.is_empty() {
            return Err(Error::from_static(EPERM));
        }
        state.place(trimmed, Body::Rpc(trimmed.to_string()))?;
        Ok(())
    }

    pub fn register_write_relay(&self, path: &str) -> Result<()> {
        let mut state = self.lock()?;
        let trimmed = path.trim_matches('/');
        if trimmed.is_empty() {
            return Err(Error::from_static(EPERM));
        }
        state.place(trimmed, Body::WriteRelay(trimmed.to_string()))?;
        Ok(())
    }

    pub fn register_log(&self, path: &str) -> Result<()> {
        let mut state = self.lock()?;
        let trimmed = path.trim_matches('/');
        if trimmed.is_empty() {
            return Err(Error::from_static(EPERM));
        }
        state.place(trimmed, Body::Log(LogBody::empty()))?;
        Ok(())
    }

    pub fn set_principal_root(&self, principal: &str, root_path: &str) -> Result<()> {
        self.set_principal_root_aname(principal, "*", root_path)
    }

    pub fn set_principal_root_aname(
        &self,
        principal: &str,
        aname: &str,
        root_path: &str,
    ) -> Result<()> {
        if principal.is_empty() {
            return Err(Error::from_static(EPERM));
        }
        if aname.is_empty() {
            return Err(Error::from_static(EPERM));
        }
        let mut state = self.lock()?;
        let root = state.lookup_path(root_path)?;
        if !matches!(state.node(root)?.body, Body::Dir(_)) {
            return Err(Error::from_static(ENOTDIR));
        }
        state.principal_roots_required = true;
        match state.principal_roots.get_mut(principal.as_bytes()) {
            Some(existing) => {
                if existing.root != root {
                    return Err(Error::from_static("principal root path mismatch"));
                }
                existing.anames.insert(aname.as_bytes().to_vec());
            }
            None => {
                let mut anames = BTreeSet::new();
                anames.insert(aname.as_bytes().to_vec());
                state.principal_roots.insert(
                    principal.as_bytes().to_vec(),
                    PrincipalRoot { root, anames },
                );
            }
        }
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

    pub fn next_request_blocking(&self) -> Result<IntakeRequest> {
        let mut state = self.lock()?;
        loop {
            if let Some(request) = state.pending.pop_front() {
                return Ok(request);
            }
            state = self
                .shared
                .1
                .wait(state)
                .map_err(|_| Error::from_static("front state poisoned"))?;
        }
    }

    pub fn complete_request(&self, prefix: &str, request_id: u64, bytes: &[u8]) -> Result<()> {
        let trimmed = prefix.trim_matches('/');
        {
            let mut state = self.lock()?;
            if let Some(slot) = state.rpc_responses.get_mut(&request_id) {
                *slot = Some(bytes.to_vec());
                drop(state);
                self.shared.1.notify_all();
                return Ok(());
            }
            if !state.is_intake_prefix(trimmed) {
                return Err(Error::from_static(ENOENT));
            }
        }
        let result_path = format!("{trimmed}/{request_id}/result");
        self.set(&result_path, bytes)
    }

    pub fn complete_write(&self, prefix: &str, request_id: u64, count: u32) -> Result<()> {
        self.complete_write_result(prefix, request_id, WriteRelayReply::Accepted(count))
    }

    pub fn reject_write(&self, prefix: &str, request_id: u64, message: &str) -> Result<()> {
        self.complete_write_result(
            prefix,
            request_id,
            WriteRelayReply::Rejected(message.to_string()),
        )
    }

    fn complete_write_result(
        &self,
        prefix: &str,
        request_id: u64,
        reply: WriteRelayReply,
    ) -> Result<()> {
        let mut state = self.lock()?;
        let relay_id = state.lookup_path(prefix)?;
        if !matches!(state.node(relay_id)?.body, Body::WriteRelay(_)) {
            return Err(Error::from_static(ENOENT));
        }
        match state.write_relay_responses.get_mut(&request_id) {
            Some(slot) => {
                *slot = Some(reply);
                drop(state);
                self.shared.1.notify_all();
                Ok(())
            }
            None => Err(Error::from_static(ENOENT)),
        }
    }

    fn read_rpc(
        &self,
        mut state: std::sync::MutexGuard<'_, State>,
        request_id: u64,
        offset: u64,
        count: u32,
        cancel: Option<&AtomicBool>,
    ) -> Result<ReadData> {
        let deadline = Instant::now() + state.wait_timeout;
        loop {
            if cancel.is_some_and(|cancel| cancel.load(Ordering::SeqCst)) {
                return Err(Error::from_static("request flushed"));
            }
            match state.rpc_responses.get(&request_id) {
                None => return Err(Error::from_static(ENOENT)),
                Some(Some(bytes)) => {
                    let start = usize::try_from(offset.min(bytes.len() as u64))
                        .map_err(|_| Error::from_static(EPERM))?;
                    let end = bytes.len().min(start.saturating_add(count as usize));
                    return Ok(ReadData::Bytes(bytes[start..end].to_vec()));
                }
                Some(None) => {}
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(Error::from_static(
                    "rpc request timed out awaiting response",
                ));
            }
            let (next, _timeout_result) = self
                .shared
                .1
                .wait_timeout(state, deadline - now)
                .map_err(|_| Error::from_static("front state poisoned"))?;
            state = next;
        }
    }

    pub(crate) fn rpc_read(
        &self,
        request_id: u64,
        offset: u64,
        count: u32,
        cancel: Option<&AtomicBool>,
    ) -> Result<ReadData> {
        let state = self.lock()?;
        self.read_rpc(state, request_id, offset, count, cancel)
    }

    fn wait_write_relay(
        &self,
        mut state: std::sync::MutexGuard<'_, State>,
        request_id: u64,
        data_len: usize,
        cancel: Option<&AtomicBool>,
    ) -> Result<u32> {
        let deadline = Instant::now() + state.wait_timeout;
        loop {
            if cancel.is_some_and(|cancel| cancel.load(Ordering::SeqCst)) {
                state.write_relay_responses.remove(&request_id);
                state.remove_pending_request(request_id);
                return Err(Error::from_static("request flushed"));
            }
            match state.write_relay_responses.remove(&request_id) {
                None => return Err(Error::from_static(ENOENT)),
                Some(Some(WriteRelayReply::Accepted(count))) => {
                    if usize::try_from(count).map_or(true, |count| count > data_len) {
                        return Err(Error::from_static(EPERM));
                    }
                    return Ok(count);
                }
                Some(Some(WriteRelayReply::Rejected(message))) => {
                    return Err(Error::from(message));
                }
                Some(None) => {
                    state.write_relay_responses.insert(request_id, None);
                }
            }
            let now = Instant::now();
            if now >= deadline {
                state.write_relay_responses.remove(&request_id);
                state.remove_pending_request(request_id);
                return Err(Error::from_static("write relay unavailable"));
            }
            let (next, _timeout_result) = self
                .shared
                .1
                .wait_timeout(state, deadline - now)
                .map_err(|_| Error::from_static("front state poisoned"))?;
            state = next;
        }
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
            session_id: self.next_session_id.fetch_add(1, Ordering::Relaxed),
            fids: BTreeMap::new(),
            open_modes: BTreeMap::new(),
            rpc_inflight: BTreeMap::new(),
        }
    }

    pub(crate) fn wake_readers(&self) {
        self.shared.1.notify_all();
    }

    pub(crate) fn max_msize(&self) -> Result<u32> {
        Ok(self.lock()?.protocol.max_msize)
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
            Body::Rpc(_) => Err(Error::from_static(EPERM)),
            Body::WriteRelay(_) => Err(Error::from_static(EPERM)),
        }
    }
}

pub(crate) enum ReadTarget {
    Node(u64),
    Rpc(u64),
}

pub struct FrontTree {
    front: Front,
    session_id: u64,
    fids: BTreeMap<Fid, FidBinding>,
    open_modes: BTreeMap<Fid, u8>,
    rpc_inflight: BTreeMap<Fid, u64>,
}

#[derive(Clone)]
struct FidBinding {
    node: u64,
    root: u64,
    session_id: u64,
    uname: Vec<u8>,
    aname: Vec<u8>,
}

impl FileTree for FrontTree {
    fn attach(&mut self, fid: Fid, uname: &[u8], aname: &[u8]) -> Result<Qid> {
        let state = self.front.lock()?;
        let root = state.attach_root_for(uname, aname)?;
        let qid = state.qid_for(root)?;
        self.fids.insert(
            fid,
            FidBinding {
                node: root,
                root,
                session_id: self.session_id,
                uname: uname.to_vec(),
                aname: aname.to_vec(),
            },
        );
        Ok(qid)
    }

    fn walk(&mut self, fid: Fid, newfid: Fid, _start: Qid, names: &[Vec<u8>]) -> Result<Vec<Qid>> {
        let state = self.front.lock()?;
        let binding = self
            .fids
            .get(&fid)
            .cloned()
            .ok_or_else(|| Error::from_static(EBADFID))?;
        let mut current = binding.node;
        let mut qids = Vec::with_capacity(names.len());
        for name in names {
            let child = if name.as_slice() == b".." {
                if current == binding.root {
                    Some(current)
                } else {
                    Some(state.node(current)?.parent)
                }
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
            self.fids.insert(
                newfid,
                FidBinding {
                    node: current,
                    ..binding
                },
            );
        }
        Ok(qids)
    }

    fn open(&mut self, fid: Fid, qid: Qid, mode: u8) -> Result<OpenFile> {
        let state = self.front.lock()?;
        let id = self
            .fids
            .get(&fid)
            .ok_or_else(|| Error::from_static(EBADFID))?
            .node;
        if state.qid_for(id)?.path != qid.path {
            return Err(Error::from_static(EBADFID));
        }
        if !open_allowed(&state.node(id)?.body, mode) {
            return Err(Error::from_static(EPERM));
        }
        self.open_modes.insert(fid, mode);
        Ok(OpenFile {
            qid,
            iounit: state.protocol.iounit,
        })
    }

    fn read(&mut self, fid: Fid, _qid: Qid, offset: u64, count: u32) -> Result<ReadData> {
        match self.read_target(fid)? {
            ReadTarget::Node(id) => self.front.read_node(id, offset, count, None),
            ReadTarget::Rpc(request_id) => self.front.rpc_read(request_id, offset, count, None),
        }
    }

    fn write(&mut self, fid: Fid, _qid: Qid, offset: u64, data: &[u8]) -> Result<u32> {
        let mut state = self.front.lock()?;
        let binding = self
            .fids
            .get(&fid)
            .cloned()
            .ok_or_else(|| Error::from_static(EBADFID))?;
        let id = binding.node;
        let open_mode = self.open_modes.get(&fid).copied().unwrap_or(0);
        let target_path = state.path_relative_to(id, binding.root)?;
        let pushed_generation = state.node(id)?.generation;
        let request_context = RequestContext::from_parts(
            &binding,
            fid,
            target_path,
            offset,
            open_mode,
            pushed_generation,
        );
        if let Body::Rpc(prefix) = &state.node(id)?.body {
            let prefix = prefix.clone();
            if let Some(previous) = self.rpc_inflight.remove(&fid) {
                state.rpc_responses.remove(&previous);
            }
            let request_id = state.next_request_id;
            state.next_request_id = state.next_request_id.saturating_add(1);
            state.rpc_responses.insert(request_id, None);
            state.pending.push_back(IntakeRequest {
                request_id,
                prefix,
                bytes: data.to_vec(),
                context: request_context,
            });
            self.rpc_inflight.insert(fid, request_id);
            drop(state);
            self.front.shared.1.notify_all();
            return u32::try_from(data.len()).map_err(|_| Error::from_static(EPERM));
        }
        if let Body::WriteRelay(prefix) = &state.node(id)?.body {
            let prefix = prefix.clone();
            let request_id = state.next_request_id;
            state.next_request_id = state.next_request_id.saturating_add(1);
            state.write_relay_responses.insert(request_id, None);
            state.pending.push_back(IntakeRequest {
                request_id,
                prefix,
                bytes: data.to_vec(),
                context: request_context,
            });
            drop(state);
            self.front.shared.1.notify_all();
            let state = self.front.lock()?;
            return self
                .front
                .wait_write_relay(state, request_id, data.len(), None);
        }
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
            prefix,
            bytes: data.to_vec(),
            context: request_context,
        });
        drop(state);
        self.front.shared.1.notify_all();
        u32::try_from(data.len()).map_err(|_| Error::from_static(EPERM))
    }

    fn stat(&mut self, qid: Qid) -> Result<Stat> {
        let state = self.front.lock()?;
        let id = state.node_id_for_qid_path(qid.path)?;
        state.stat_for(id)
    }

    fn clunk(&mut self, fid: Fid, _qid: Qid) -> Result<()> {
        self.fids.remove(&fid);
        self.open_modes.remove(&fid);
        if let Some(request_id) = self.rpc_inflight.remove(&fid) {
            if let Ok(mut state) = self.front.lock() {
                state.rpc_responses.remove(&request_id);
            }
        }
        Ok(())
    }
}

impl FrontTree {
    pub(crate) fn read_target(&self, fid: Fid) -> Result<ReadTarget> {
        let id = self
            .fids
            .get(&fid)
            .ok_or_else(|| Error::from_static(EBADFID))?
            .node;
        let state = self.front.lock()?;
        if matches!(state.node(id)?.body, Body::Rpc(_)) {
            let request_id = self
                .rpc_inflight
                .get(&fid)
                .copied()
                .ok_or_else(|| Error::from_static("rpc read before write on this fid"))?;
            return Ok(ReadTarget::Rpc(request_id));
        }
        Ok(ReadTarget::Node(id))
    }

    pub(crate) fn front(&self) -> Front {
        self.front.clone()
    }
}

impl Drop for FrontTree {
    fn drop(&mut self) {
        if self.rpc_inflight.is_empty() {
            return;
        }
        if let Ok(mut state) = self.front.lock() {
            for (_, request_id) in std::mem::take(&mut self.rpc_inflight) {
                state.rpc_responses.remove(&request_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, thread};

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
    fn pushed_file_uses_brain_owned_qid_and_version() -> Result<()> {
        let front = Front::new();
        front.set_pushed_file(
            "market/status",
            b"first",
            PushedFileMetadata {
                qid_path: 9001,
                qid_version: 44,
                generation: 100,
                visibility_class: "runtime-reader".to_string(),
                wake_token: "wake:status".to_string(),
            },
        )?;
        front.set_pushed_file(
            "market/status",
            b"second",
            PushedFileMetadata {
                qid_path: 9001,
                qid_version: 45,
                generation: 101,
                visibility_class: "runtime-reader".to_string(),
                wake_token: "wake:status".to_string(),
            },
        )?;
        let mut tree = front.tree();
        tree.attach(1, b"claude", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["market", "status"]);
        assert_eq!(qids[1].path, 9001);
        assert_eq!(qids[1].version, 45);
        let stat = tree.stat(qids[1])?;
        assert_eq!(stat.qid.path, 9001);
        assert_eq!(stat.qid.version, 45);
        let data = tree.read(2, qids[1], 0, 4096)?;
        assert_eq!(data, ReadData::Bytes(b"second".to_vec()));
        Ok(())
    }

    #[test]
    fn protocol_limits_control_open_iounit() -> Result<()> {
        let front = Front::new();
        front.set_protocol_limits(65_536, 4096)?;
        front.set("market/status", b"x")?;
        let mut tree = front.tree();
        tree.attach(1, b"claude", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["market", "status"]);
        let opened = tree.open(2, qids[1], OREAD)?;
        assert_eq!(opened.iounit, 4096);
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
        assert_eq!(request.prefix, "queries");
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
    fn intake_blocking_request_wait_wakes_on_write() -> Result<()> {
        let front = Front::new();
        front.register_intake("queries")?;
        let worker_front = front.clone();
        let worker = thread::spawn(move || worker_front.next_request_blocking());
        let mut tree = front.tree();
        tree.attach(1, b"claude", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["queries", "new"]);
        tree.open(2, qids[1], 1)?;
        tree.write(2, qids[1], 0, b"blocked wait wakes")?;
        let request = worker.join().expect("worker joins")?;
        assert_eq!(request.request_id, 1);
        assert_eq!(request.prefix, "queries");
        assert_eq!(request.bytes, b"blocked wait wakes");
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

    #[test]
    fn root_directory_reports_dmdir_and_lists_top_level_children() -> Result<()> {
        let front = Front::new();
        front.set("manifest", b"m")?;
        front.set("state", b"s")?;
        front.register_rpc("queries")?;
        front.append_event("events", b"e\n")?;
        let mut tree = front.tree();
        let root_qid = tree.attach(1, b"claude", b"/")?;
        let stat = tree.stat(root_qid)?;
        assert_ne!(stat.mode & DMDIR, 0);
        assert_eq!(stat.name, b".".to_vec());
        match tree.read(1, root_qid, 0, 4096)? {
            ReadData::Directory(stats) => {
                let mut names: Vec<Vec<u8>> = stats.iter().map(|stat| stat.name.clone()).collect();
                names.sort();
                assert_eq!(
                    names,
                    vec![
                        b"events".to_vec(),
                        b"manifest".to_vec(),
                        b"queries".to_vec(),
                        b"state".to_vec(),
                    ]
                );
            }
            ReadData::Bytes(_) => panic!("expected directory listing for root"),
        }
        Ok(())
    }

    #[test]
    fn pushed_principal_roots_select_views_and_fail_closed() -> Result<()> {
        let front = Front::new();
        front.set("views/alice/status", b"alice-visible")?;
        front.set("views/bob/status", b"bob-visible")?;
        front.set_principal_root_aname("alice", "/", "views/alice")?;

        let mut alice = front.tree();
        alice.attach(1, b"alice", b"/")?;
        let status = walk_to(&mut alice, 1, 2, &["status"]);
        assert_eq!(status.len(), 1);
        let data = alice.read(2, status[0], 0, 4096)?;
        assert_eq!(data, ReadData::Bytes(b"alice-visible".to_vec()));

        let escape = walk_to(&mut alice, 1, 3, &["..", "bob", "status"]);
        assert_eq!(escape.len(), 1);

        let mut bob = front.tree();
        let error = bob
            .attach(1, b"bob", b"/")
            .expect_err("principal without pushed root must fail closed");
        assert_eq!(error.message(), b"principal root unavailable");

        let mut wrong_aname = front.tree();
        let error = wrong_aname
            .attach(1, b"alice", b"not-admitted")
            .expect_err("principal without admitted aname must fail closed");
        assert_eq!(error.message(), b"principal aname unavailable");
        Ok(())
    }

    #[test]
    fn write_relay_returns_count_after_brain_accepts() -> Result<()> {
        let front = Front::new();
        front.register_write_relay("control")?;
        front.set_wait_timeout(Duration::from_secs(5))?;
        let writer_front = front.clone();
        let (done_tx, done_rx) = mpsc::channel();
        let writer = thread::spawn(move || {
            let mut tree = writer_front.tree();
            tree.attach(1, b"alice", b"/")?;
            let qids = walk_to(&mut tree, 1, 2, &["control"]);
            tree.open(2, qids[0], OWRITE)?;
            let result = tree.write(2, qids[0], 0, b"#M(\"command\" \"restart\")");
            done_tx.send(result).expect("send writer result");
            Ok::<(), Error>(())
        });

        let request = front
            .next_request(Duration::from_millis(200))?
            .expect("write relay request");
        assert_eq!(request.prefix, "control");
        assert_eq!(request.bytes, b"#M(\"command\" \"restart\")");
        assert!(done_rx.recv_timeout(Duration::from_millis(50)).is_err());

        front.complete_write(
            "control",
            request.request_id,
            u32::try_from(request.bytes.len()).expect("request length"),
        )?;
        let wrote = done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("writer should finish")?;
        assert_eq!(wrote as usize, request.bytes.len());
        writer.join().expect("writer join")?;
        Ok(())
    }

    #[test]
    fn write_relay_reports_unavailable_when_brain_is_absent() -> Result<()> {
        let front = Front::new();
        front.register_write_relay("control")?;
        front.set_wait_timeout(Duration::from_millis(20))?;
        let mut tree = front.tree();
        tree.attach(1, b"alice", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["control"]);
        tree.open(2, qids[0], OWRITE)?;
        let error = tree
            .write(2, qids[0], 0, b"#M(\"command\" \"restart\")")
            .expect_err("write relay without brain must fail");
        assert_eq!(error.message(), b"write relay unavailable");
        assert!(front.next_request(Duration::from_millis(0))?.is_none());
        Ok(())
    }

    #[test]
    fn write_relay_can_return_brain_denial() -> Result<()> {
        let front = Front::new();
        front.register_write_relay("control")?;
        front.set_wait_timeout(Duration::from_secs(5))?;
        let writer_front = front.clone();
        let (done_tx, done_rx) = mpsc::channel();
        let writer = thread::spawn(move || {
            let mut tree = writer_front.tree();
            tree.attach(1, b"alice", b"/")?;
            let qids = walk_to(&mut tree, 1, 2, &["control"]);
            tree.open(2, qids[0], OWRITE)?;
            let result = tree.write(2, qids[0], 0, b"#M(\"command\" \"restart\")");
            done_tx.send(result).expect("send writer result");
            Ok::<(), Error>(())
        });

        let request = front
            .next_request(Duration::from_millis(200))?
            .expect("write relay request");
        front.reject_write("control", request.request_id, "authority denied")?;
        let error = done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("writer should finish")
            .expect_err("brain denial should reach writer");
        assert_eq!(error.message(), b"authority denied");
        writer.join().expect("writer join")?;
        Ok(())
    }

    #[test]
    fn dropping_tree_abandons_pending_rpc_response() -> Result<()> {
        let front = Front::new();
        front.register_rpc("queries")?;
        let mut tree = front.tree();
        tree.attach(1, b"alice", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["queries"]);
        tree.open(2, qids[0], ORDWR)?;
        tree.write(2, qids[0], 0, b"find markets")?;
        let request = front
            .next_request(Duration::from_millis(200))?
            .expect("rpc request");
        drop(tree);

        let error = front
            .complete_request("queries", request.request_id, b"late")
            .expect_err("closed rpc request must not become an intake result");
        assert_eq!(error.message(), ENOENT.as_bytes());

        let mut verifier = front.tree();
        verifier.attach(1, b"alice", b"/")?;
        let qids = walk_to(
            &mut verifier,
            1,
            2,
            &["queries", &request.request_id.to_string(), "result"],
        );
        assert!(qids.len() < 3);
        Ok(())
    }

    #[test]
    fn register_log_declares_an_empty_walkable_log() -> Result<()> {
        let front = Front::new();
        front.register_log("events")?;
        let mut tree = front.tree();
        tree.attach(1, b"claude", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["events"]);
        assert_eq!(qids.len(), 1);
        let stat = tree.stat(qids[0])?;
        assert_eq!(stat.length, 0);
        assert_eq!(stat.mode & DMDIR, 0);
        tree.open(2, qids[0], OREAD)?;
        front.append_event("events", b"first\n")?;
        let data = tree.read(2, qids[0], 0, 4096)?;
        assert_eq!(data, ReadData::Bytes(b"first\n".to_vec()));
        Ok(())
    }

    #[test]
    fn rpc_node_only_opens_read_write() -> Result<()> {
        let front = Front::new();
        front.register_rpc("queries")?;
        let mut tree = front.tree();
        tree.attach(1, b"claude", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["queries"]);
        assert!(tree.open(2, qids[0], ORDWR).is_ok());
        assert!(tree.open(2, qids[0], OREAD).is_err());
        assert!(tree.open(2, qids[0], OWRITE).is_err());
        Ok(())
    }

    #[test]
    fn rpc_single_fid_request_response_roundtrip() -> Result<()> {
        let front = Front::new();
        front.register_rpc("queries")?;
        let mut tree = front.tree();
        tree.attach(1, b"claude", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["queries"]);
        tree.open(2, qids[0], ORDWR)?;
        let written = tree.write(2, qids[0], 0, b"find markets")?;
        assert_eq!(written as usize, "find markets".len());
        let request = front
            .next_request(Duration::from_millis(200))?
            .expect("a pending rpc request");
        assert_eq!(request.prefix, "queries");
        assert_eq!(request.bytes, b"find markets");
        front.complete_request("queries", request.request_id, b"{\"hits\":2}")?;
        let response = tree.read(2, qids[0], 0, 4096)?;
        assert_eq!(response, ReadData::Bytes(b"{\"hits\":2}".to_vec()));
        let tail = tree.read(2, qids[0], 6, 4096)?;
        assert_eq!(tail, ReadData::Bytes(b"\":2}".to_vec()));
        Ok(())
    }

    #[test]
    fn rpc_request_carries_the_registered_path() -> Result<()> {
        let front = Front::new();
        front.register_rpc("queries")?;
        front.register_rpc("candidates")?;
        let mut tree = front.tree();
        tree.attach(1, b"claude", b"/")?;
        let query_qids = walk_to(&mut tree, 1, 2, &["queries"]);
        tree.open(2, query_qids[0], ORDWR)?;
        tree.write(2, query_qids[0], 0, b"browse")?;
        let candidate_qids = walk_to(&mut tree, 1, 3, &["candidates"]);
        tree.open(3, candidate_qids[0], ORDWR)?;
        tree.write(3, candidate_qids[0], 0, b"scan")?;
        let first = front
            .next_request(Duration::from_millis(200))?
            .expect("first request");
        let second = front
            .next_request(Duration::from_millis(200))?
            .expect("second request");
        assert_eq!(first.prefix, "queries");
        assert_eq!(first.bytes, b"browse");
        assert_eq!(second.prefix, "candidates");
        assert_eq!(second.bytes, b"scan");
        Ok(())
    }

    #[test]
    fn rpc_read_before_write_is_an_error() -> Result<()> {
        let front = Front::new();
        front.register_rpc("queries")?;
        let mut tree = front.tree();
        tree.attach(1, b"claude", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["queries"]);
        tree.open(2, qids[0], ORDWR)?;
        assert!(tree.read(2, qids[0], 0, 4096).is_err());
        Ok(())
    }

    #[test]
    fn rpc_second_request_on_same_fid_replaces_the_first() -> Result<()> {
        let front = Front::new();
        front.register_rpc("queries")?;
        let mut tree = front.tree();
        tree.attach(1, b"claude", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["queries"]);
        tree.open(2, qids[0], ORDWR)?;
        let _ = tree.write(2, qids[0], 0, b"first")?;
        let first = front
            .next_request(Duration::from_millis(200))?
            .expect("first request");
        front.complete_request("queries", first.request_id, b"one")?;
        let _ = tree.write(2, qids[0], 0, b"second")?;
        let second = front
            .next_request(Duration::from_millis(200))?
            .expect("second request");
        assert_eq!(second.prefix, "queries");
        assert_eq!(second.bytes, b"second");
        front.complete_request("queries", second.request_id, b"two")?;
        let response = tree.read(2, qids[0], 0, 4096)?;
        assert_eq!(response, ReadData::Bytes(b"two".to_vec()));
        Ok(())
    }
}
