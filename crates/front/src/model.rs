use r9p::codec::MAX_MSIZE;
use r9p::error::{Error, Result, EEXIST, ENOENT, ENOTDIR, EPERM};
use r9p::fid::Fid;
use r9p::mode;
use r9p::qid::{Qid, DMDIR, QTDIR, QTFILE};
use r9p::stat::Stat;
use r9p::{ORCLOSE, ORDWR, OREAD, OTRUNC, OWRITE};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ops::{Deref, DerefMut};
use std::time::Duration;

pub(crate) const ROOT_ID: u64 = 0;

pub const DEFAULT_LOG_CAPACITY: usize = 1 << 20;

pub const DEFAULT_IOUNIT: u32 = 4096;

#[derive(Default)]
pub(crate) struct DirectoryBody {
    pub(crate) children: BTreeMap<Vec<u8>, u64>,
    pub(crate) read_relay: Option<String>,
}

impl Deref for DirectoryBody {
    type Target = BTreeMap<Vec<u8>, u64>;

    fn deref(&self) -> &Self::Target {
        &self.children
    }
}

impl DerefMut for DirectoryBody {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.children
    }
}

pub(crate) enum Body {
    Dir(DirectoryBody),
    File(Vec<u8>),
    Log(LogBody),
    IntakeNew(u64),
    Rpc(String),
    ReadRelay(String),
    SnapshotReadRelay(String),
    WriteRelay(String),
}

pub(crate) struct LogBody {
    pub(crate) entries: VecDeque<Vec<u8>>,
    pub(crate) start: u64,
    pub(crate) retained: usize,
}

impl LogBody {
    pub(crate) fn new(bytes: Vec<u8>) -> Self {
        let retained = bytes.len();
        let mut entries = VecDeque::new();
        entries.push_back(bytes);
        Self {
            entries,
            start: 0,
            retained,
        }
    }

    pub(crate) fn empty() -> Self {
        Self::empty_at(0)
    }

    pub(crate) fn empty_at(start: u64) -> Self {
        Self {
            entries: VecDeque::new(),
            start,
            retained: 0,
        }
    }

    pub(crate) fn end(&self) -> u64 {
        self.start + self.retained as u64
    }

    pub(crate) fn append(&mut self, bytes: Vec<u8>, capacity: usize) {
        self.retained += bytes.len();
        self.entries.push_back(bytes);
        while self.retained > capacity && self.entries.len() > 1 {
            if let Some(oldest) = self.entries.pop_front() {
                self.start += oldest.len() as u64;
                self.retained -= oldest.len();
            }
        }
    }

    pub(crate) fn read(&self, offset: u64, count: usize) -> Vec<u8> {
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

pub struct CreateRelayRequest {
    pub request_id: u64,
    pub prefix: String,
    pub name: String,
    pub perm: u32,
    pub mode: u8,
    pub context: RequestContext,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestContext {
    pub principal_id: String,
    pub uname: String,
    pub aname: String,
    pub session_id: u64,
    pub fid: Fid,
    pub front_path: String,
    pub target_path: String,
    pub offset: u64,
    pub count: u32,
    pub open_mode: u8,
    pub pushed_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushedEntryMetadata {
    pub qid_path: u64,
    pub qid_version: u32,
    pub generation: u64,
    pub visibility_class: String,
    pub freshness_ref: String,
    pub wake_token: String,
}

pub type PushedFileMetadata = PushedEntryMetadata;

pub type PushedDirectoryMetadata = PushedEntryMetadata;

pub(crate) enum WriteRelayReply {
    Accepted(u32),
    Rejected(String),
}

pub(crate) enum RequestReply {
    Accepted(Vec<u8>),
    DirectoryAccepted { names: Vec<Vec<u8>>, eof: bool },
    Rejected(String),
}

pub(crate) enum RemoveRelayReply {
    Accepted,
    Rejected(String),
}

pub(crate) enum WstatRelayReply {
    Accepted,
    Rejected(String),
}

pub(crate) enum CreateRelayReply {
    Accepted {
        qtype: u8,
        qid_version: u32,
        qid_path: u64,
    },
    Rejected(String),
}

pub(crate) struct Node {
    pub(crate) name: Vec<u8>,
    pub(crate) parent: u64,
    pub(crate) qid_path: u64,
    pub(crate) version: u32,
    pub(crate) generation: u64,
    pub(crate) visibility_class: Option<String>,
    pub(crate) freshness_ref: Option<String>,
    pub(crate) wake_token: Option<String>,
    pub(crate) create_relay: Option<String>,
    pub(crate) write_relay: Option<String>,
    pub(crate) remove_relay: Option<String>,
    pub(crate) wstat_relay: Option<String>,
    pub(crate) body: Body,
}

pub(crate) struct State {
    pub(crate) nodes: BTreeMap<u64, Node>,
    pub(crate) qid_index: BTreeMap<u64, u64>,
    pub(crate) next_id: u64,
    pub(crate) next_request_id: u64,
    pub(crate) intakes: BTreeMap<u64, Intake>,
    pub(crate) pending: VecDeque<IntakeRequest>,
    pub(crate) create_pending: VecDeque<CreateRelayRequest>,
    pub(crate) rpc_responses: BTreeMap<u64, Option<RequestReply>>,
    pub(crate) response_prefixes: BTreeMap<u64, String>,
    pub(crate) directory_response_requests: BTreeSet<u64>,
    pub(crate) create_relay_responses: BTreeMap<u64, Option<CreateRelayReply>>,
    pub(crate) write_relay_responses: BTreeMap<u64, Option<WriteRelayReply>>,
    pub(crate) remove_relay_responses: BTreeMap<u64, Option<RemoveRelayReply>>,
    pub(crate) wstat_relay_responses: BTreeMap<u64, Option<WstatRelayReply>>,
    pub(crate) write_relay_prefixes: BTreeSet<String>,
    pub(crate) remove_relay_prefixes: BTreeSet<String>,
    pub(crate) wstat_relay_prefixes: BTreeSet<String>,
    pub(crate) principal_roots_required: bool,
    pub(crate) principal_roots: BTreeMap<Vec<u8>, PrincipalRoot>,
    pub(crate) wait_timeout: Duration,
    pub(crate) log_capacity: usize,
    pub(crate) protocol: ProtocolConfig,
}

pub(crate) struct Intake {
    pub(crate) prefix: String,
}

pub(crate) struct PrincipalRoot {
    pub(crate) root: u64,
    pub(crate) root_path: String,
    pub(crate) principal_id: String,
    pub(crate) anames: BTreeSet<Vec<u8>>,
}

#[derive(Clone, Copy)]
pub(crate) struct ProtocolConfig {
    pub(crate) max_msize: u32,
    pub(crate) iounit: u32,
}

impl Default for ProtocolConfig {
    fn default() -> Self {
        Self {
            max_msize: MAX_MSIZE,
            iounit: DEFAULT_IOUNIT,
        }
    }
}

impl State {
    pub(crate) fn new() -> Self {
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
                freshness_ref: None,
                wake_token: None,
                create_relay: None,
                write_relay: None,
                remove_relay: None,
                wstat_relay: None,
                body: Body::Dir(DirectoryBody::default()),
            },
        );
        Self {
            nodes,
            qid_index,
            next_id: 1,
            next_request_id: 1,
            intakes: BTreeMap::new(),
            pending: VecDeque::new(),
            create_pending: VecDeque::new(),
            rpc_responses: BTreeMap::new(),
            response_prefixes: BTreeMap::new(),
            directory_response_requests: BTreeSet::new(),
            create_relay_responses: BTreeMap::new(),
            write_relay_responses: BTreeMap::new(),
            remove_relay_responses: BTreeMap::new(),
            wstat_relay_responses: BTreeMap::new(),
            write_relay_prefixes: BTreeSet::new(),
            remove_relay_prefixes: BTreeSet::new(),
            wstat_relay_prefixes: BTreeSet::new(),
            principal_roots_required: false,
            principal_roots: BTreeMap::new(),
            wait_timeout: Duration::from_secs(30),
            log_capacity: DEFAULT_LOG_CAPACITY,
            protocol: ProtocolConfig::default(),
        }
    }

    pub(crate) fn node(&self, id: u64) -> Result<&Node> {
        self.nodes
            .get(&id)
            .ok_or_else(|| Error::from_static(ENOENT))
    }

    pub(crate) fn qid_for(&self, id: u64) -> Result<Qid> {
        let node = self.node(id)?;
        let qtype = match node.body {
            Body::Dir(_) => QTDIR,
            _ => QTFILE,
        };
        Ok(Qid::new(qtype, node.version, node.qid_path))
    }

    pub(crate) fn node_id_for_qid_path(&self, qid_path: u64) -> Result<u64> {
        self.qid_index
            .get(&qid_path)
            .copied()
            .ok_or_else(|| Error::from_static(ENOENT))
    }

    pub(crate) fn replace_qid_path(&mut self, id: u64, qid_path: u64) -> Result<()> {
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

    pub(crate) fn apply_pushed_metadata(
        &mut self,
        id: u64,
        metadata: &PushedEntryMetadata,
    ) -> Result<()> {
        self.replace_qid_path(id, metadata.qid_path)?;
        if let Some(node) = self.nodes.get_mut(&id) {
            node.version = metadata.qid_version;
            node.generation = metadata.generation;
            node.visibility_class = Some(metadata.visibility_class.clone());
            node.freshness_ref = Some(metadata.freshness_ref.clone());
            node.wake_token = Some(metadata.wake_token.clone());
        }
        Ok(())
    }

    pub(crate) fn stat_for(&self, id: u64) -> Result<Stat> {
        let node = self.node(id)?;
        let qid = self.qid_for(id)?;
        let (mode, length) = match &node.body {
            Body::Dir(_) => (DMDIR | 0o555, 0u64),
            Body::File(bytes) => {
                let mode = if node.write_relay.is_some() {
                    0o666u32
                } else {
                    0o444u32
                };
                (mode, bytes.len() as u64)
            }
            Body::Log(log) => (0o444u32, log.end()),
            Body::IntakeNew(_) => (0o222u32, 0u64),
            Body::Rpc(_) => (0o600u32, 0u64),
            Body::ReadRelay(_) | Body::SnapshotReadRelay(_) => (0o444u32, 0u64),
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

    pub(crate) fn ensure_dir(&mut self, parent: u64, name: &[u8]) -> Result<u64> {
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
                freshness_ref: None,
                wake_token: None,
                create_relay: None,
                write_relay: None,
                remove_relay: None,
                wstat_relay: None,
                body: Body::Dir(DirectoryBody::default()),
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

    pub(crate) fn ensure_path_dir(&mut self, path: &str) -> Result<u64> {
        let segments = split_path(path)?;
        let mut current = ROOT_ID;
        for segment in segments {
            current = self.ensure_dir(current, &segment)?;
        }
        Ok(current)
    }

    pub(crate) fn place(&mut self, path: &str, body: Body) -> Result<u64> {
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
                        freshness_ref: None,
                        wake_token: None,
                        create_relay: None,
                        write_relay: None,
                        remove_relay: None,
                        wstat_relay: None,
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

    pub(crate) fn place_pushed_file(
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
                if !matches!(self.node(id)?.body, Body::File(_) | Body::WriteRelay(_)) {
                    return Err(Error::from_static(EPERM));
                }
                self.apply_pushed_metadata(id, &metadata)?;
                if let Some(node) = self.nodes.get_mut(&id) {
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
                        freshness_ref: Some(metadata.freshness_ref),
                        wake_token: Some(metadata.wake_token),
                        create_relay: None,
                        write_relay: None,
                        remove_relay: None,
                        wstat_relay: None,
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

    pub(crate) fn place_pushed_directory(
        &mut self,
        path: &str,
        metadata: PushedDirectoryMetadata,
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
                if !matches!(self.node(id)?.body, Body::Dir(_)) {
                    return Err(Error::from_static(EPERM));
                }
                self.apply_pushed_metadata(id, &metadata)?;
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
                        freshness_ref: Some(metadata.freshness_ref),
                        wake_token: Some(metadata.wake_token),
                        create_relay: None,
                        write_relay: None,
                        remove_relay: None,
                        wstat_relay: None,
                        body: Body::Dir(DirectoryBody::default()),
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

    pub(crate) fn insert_created_relay_node(
        &mut self,
        parent_id: u64,
        name: &str,
        qid: Qid,
        generation: u64,
        create_prefix: String,
    ) -> Result<u64> {
        if self.qid_index.contains_key(&qid.path) {
            return Err(Error::from_static("qid path already in use"));
        }
        let segments = split_path(name)?;
        let (leaf, dirs) = segments
            .split_last()
            .ok_or_else(|| Error::from_static(EPERM))?;
        let mut parent = parent_id;
        for dir in dirs {
            parent = self.ensure_dir(parent, dir)?;
        }
        let existing = match &self.node(parent)?.body {
            Body::Dir(children) => children.get(leaf.as_slice()).copied(),
            _ => return Err(Error::from_static(ENOTDIR)),
        };
        if existing.is_some() {
            return Err(Error::from_static(EEXIST));
        }
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let write_relay = if qid.qtype & QTDIR != 0 {
            None
        } else {
            Some(create_prefix.clone())
        };
        let body = if qid.qtype & QTDIR != 0 {
            Body::Dir(DirectoryBody::default())
        } else {
            Body::WriteRelay(create_prefix)
        };
        self.nodes.insert(
            id,
            Node {
                name: leaf.clone(),
                parent,
                qid_path: qid.path,
                version: qid.version,
                generation,
                visibility_class: None,
                freshness_ref: None,
                wake_token: None,
                create_relay: None,
                write_relay,
                remove_relay: None,
                wstat_relay: None,
                body,
            },
        );
        self.qid_index.insert(qid.path, id);
        if let Some(Node {
            body: Body::Dir(children),
            ..
        }) = self.nodes.get_mut(&parent)
        {
            children.insert(leaf.clone(), id);
        }
        Ok(id)
    }

    pub(crate) fn remove_subtree_if_exists(&mut self, path: &str) -> Result<()> {
        let Some(id) = self.lookup_optional_path(path)? else {
            return Ok(());
        };
        if id == ROOT_ID {
            return Err(Error::from_static(EPERM));
        }
        let parent = self.node(id)?.parent;
        let name = self.node(id)?.name.clone();
        if let Some(Node {
            body: Body::Dir(children),
            ..
        }) = self.nodes.get_mut(&parent)
        {
            children.remove(name.as_slice());
        }
        self.remove_node_recursive(id);
        Ok(())
    }

    pub(crate) fn retain_subtree_paths(
        &mut self,
        root_path: &str,
        paths: &[String],
    ) -> Result<bool> {
        let canonical_root = canonical_root_path(root_path)?;
        let root_segments = canonical_path_segments(&canonical_root)?;
        let root = self.lookup_path(&canonical_root)?;
        if !matches!(self.node(root)?.body, Body::Dir(_)) {
            return Err(Error::from_static(ENOTDIR));
        }

        let mut retained = BTreeSet::from([root]);
        for path in paths {
            let canonical_path = canonical_root_path(path)?;
            let path_segments = canonical_path_segments(&canonical_path)?;
            if !path_segments.starts_with(&root_segments) {
                return Err(Error::from_static(EPERM));
            }

            let mut current = self.lookup_path(&canonical_path)?;
            loop {
                retained.insert(current);
                if current == root {
                    break;
                }
                let parent = self.node(current)?.parent;
                if parent == current {
                    return Err(Error::from_static(EPERM));
                }
                current = parent;
            }
        }

        let mut stale = Vec::new();
        self.collect_stale_subtrees(root, &retained, &mut stale)?;
        for (parent, name, id) in &stale {
            if let Some(Node {
                body: Body::Dir(children),
                ..
            }) = self.nodes.get_mut(parent)
            {
                children.remove(name.as_slice());
            }
            self.remove_node_recursive(*id);
        }

        if !stale.is_empty() {
            let nodes = &self.nodes;
            self.principal_roots
                .retain(|_uname, binding| nodes.contains_key(&binding.root));
        }
        Ok(!stale.is_empty())
    }

    fn collect_stale_subtrees(
        &self,
        parent: u64,
        retained: &BTreeSet<u64>,
        stale: &mut Vec<(u64, Vec<u8>, u64)>,
    ) -> Result<()> {
        let children = match &self.node(parent)?.body {
            Body::Dir(children) => children
                .iter()
                .map(|(name, id)| (name.clone(), *id))
                .collect::<Vec<_>>(),
            _ => return Err(Error::from_static(ENOTDIR)),
        };
        for (name, child) in children {
            if retained.contains(&child) {
                if matches!(self.node(child)?.body, Body::Dir(_)) {
                    self.collect_stale_subtrees(child, retained, stale)?;
                }
            } else {
                stale.push((parent, name, child));
            }
        }
        Ok(())
    }

    pub(crate) fn remove_node_recursive(&mut self, id: u64) {
        let Some(node) = self.nodes.remove(&id) else {
            return;
        };
        self.qid_index.remove(&node.qid_path);
        self.intakes.remove(&id);
        if let Body::Dir(children) = node.body {
            for child in children.values() {
                self.remove_node_recursive(*child);
            }
        }
    }

    pub(crate) fn lookup_optional_path(&self, path: &str) -> Result<Option<u64>> {
        let trimmed = path.trim_matches('/');
        if trimmed.is_empty() {
            return Ok(Some(ROOT_ID));
        }
        let mut current = ROOT_ID;
        for segment in split_path(trimmed)? {
            let node = self.node(current)?;
            let child = match &node.body {
                Body::Dir(children) => children.get(segment.as_slice()).copied(),
                _ => return Err(Error::from_static(ENOTDIR)),
            };
            let Some(next) = child else {
                return Ok(None);
            };
            current = next;
        }
        Ok(Some(current))
    }

    pub(crate) fn lookup_path(&self, path: &str) -> Result<u64> {
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

    pub(crate) fn is_intake_prefix(&self, prefix: &str) -> bool {
        self.intakes.values().any(|intake| intake.prefix == prefix)
    }

    pub(crate) fn remove_pending_request(&mut self, request_id: u64) {
        self.pending
            .retain(|request| request.request_id != request_id);
    }

    pub(crate) fn remove_response_request(&mut self, request_id: u64) {
        self.rpc_responses.remove(&request_id);
        self.response_prefixes.remove(&request_id);
        self.directory_response_requests.remove(&request_id);
        self.remove_pending_request(request_id);
    }

    pub(crate) fn pop_pending_for_prefix(&mut self, prefix: &str) -> Option<IntakeRequest> {
        let index = self
            .pending
            .iter()
            .position(|request| request.prefix == prefix)?;
        self.pending.remove(index)
    }

    pub(crate) fn pop_create_pending_for_prefix(
        &mut self,
        prefix: &str,
    ) -> Option<CreateRelayRequest> {
        let index = self
            .create_pending
            .iter()
            .position(|request| request.prefix == prefix)?;
        self.create_pending.remove(index)
    }

    pub(crate) fn path_relative_to(&self, id: u64, root: u64) -> Result<String> {
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

    pub(crate) fn attach_root_for(&self, uname: &[u8], aname: &[u8]) -> Result<u64> {
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

pub(crate) fn split_path(path: &str) -> Result<Vec<Vec<u8>>> {
    let trimmed = path.trim_matches('/');
    if trimmed.is_empty() {
        return Err(Error::from_static(EPERM));
    }
    Ok(trimmed
        .split('/')
        .map(|segment| segment.as_bytes().to_vec())
        .collect())
}

pub(crate) fn canonical_root_path(path: &str) -> Result<String> {
    let trimmed = path.trim_matches('/');
    if trimmed.is_empty() {
        return Ok("/".to_string());
    }
    let _ = split_path(trimmed)?;
    Ok(trimmed.to_string())
}

fn canonical_path_segments(path: &str) -> Result<Vec<Vec<u8>>> {
    if path == "/" {
        Ok(Vec::new())
    } else {
        split_path(path)
    }
}

pub(crate) fn normalise_request_prefix(prefix: &str) -> Result<String> {
    let trimmed = prefix.trim_matches('/');
    if trimmed.is_empty() {
        return Err(Error::from_static(EPERM));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn created_child_path(parent_path: &str, name: &str) -> String {
    if parent_path == "/" {
        format!("/{name}")
    } else {
        format!("{}/{}", parent_path.trim_end_matches('/'), name)
    }
}

pub(crate) fn open_allowed(node: &Node, mode: u8) -> bool {
    if !mode::is_valid(mode) || mode & ORCLOSE != 0 {
        return false;
    }
    if node.write_relay.is_some() && mode & mode::ACCESS_MASK == OWRITE {
        return true;
    }
    if mode & OTRUNC != 0 {
        return false;
    }
    match &node.body {
        Body::Dir(_)
        | Body::File(_)
        | Body::Log(_)
        | Body::ReadRelay(_)
        | Body::SnapshotReadRelay(_) => mode & mode::ACCESS_MASK == OREAD,
        Body::IntakeNew(_) | Body::WriteRelay(_) => mode & mode::ACCESS_MASK == OWRITE,
        Body::Rpc(_) => mode & mode::ACCESS_MASK == ORDWR,
    }
}
