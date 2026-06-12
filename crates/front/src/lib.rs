use r9p::error::{Error, Result};
use r9p::fid::Fid;
use r9p::qid::{Qid, QTDIR, QTFILE};
use r9p::server::{FileTree, OpenFile, ReadData};
use r9p::stat::Stat;
use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex};

const EBADFID: &str = "unknown fid";
const ENOENT: &str = "file does not exist";
const EPERM: &str = "permission denied";
const ENOTDIR: &str = "not a directory";

const ROOT_ID: u64 = 0;

enum Body {
    Dir(BTreeMap<Vec<u8>, u64>),
    File(Vec<u8>),
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
    fids: BTreeMap<Fid, u64>,
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
            fids: BTreeMap::new(),
        }
    }

    fn node(&self, id: u64) -> Result<&Node> {
        self.nodes.get(&id).ok_or_else(|| Error::from_static(ENOENT))
    }

    fn qid_for(&self, id: u64) -> Result<Qid> {
        let node = self.node(id)?;
        let qtype = match node.body {
            Body::Dir(_) => QTDIR,
            Body::File(_) => QTFILE,
        };
        Ok(Qid::new(qtype, node.version, id))
    }

    fn stat_for(&self, id: u64) -> Result<Stat> {
        let node = self.node(id)?;
        let qid = self.qid_for(id)?;
        let (mode, length) = match &node.body {
            Body::Dir(_) => (0o040555u32, 0u64),
            Body::File(bytes) => (0o444u32, bytes.len() as u64),
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
                    Body::File(_) => Err(Error::from_static(ENOTDIR)),
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

    fn set_file(&mut self, path: &str, bytes: Vec<u8>) -> Result<()> {
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
            Body::File(_) => return Err(Error::from_static(ENOTDIR)),
        };
        match existing {
            Some(id) => match self.nodes.get_mut(&id) {
                Some(node) => match &mut node.body {
                    Body::File(_) => {
                        node.version = node.version.wrapping_add(1);
                        node.body = Body::File(bytes);
                        Ok(())
                    }
                    Body::Dir(_) => Err(Error::from_static(EPERM)),
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
                        body: Body::File(bytes),
                    },
                );
                if let Some(Node {
                    body: Body::Dir(children),
                    ..
                }) = self.nodes.get_mut(&parent)
                {
                    children.insert(last.clone(), id);
                }
                Ok(())
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
        self.lock()?.set_file(path, bytes.to_vec())?;
        self.shared.1.notify_all();
        Ok(())
    }

    pub fn tree(&self) -> FrontTree {
        FrontTree {
            front: self.clone(),
        }
    }
}

pub struct FrontTree {
    front: Front,
}

impl FileTree for FrontTree {
    fn attach(&mut self, fid: Fid, _uname: &[u8], _aname: &[u8]) -> Result<Qid> {
        let mut state = self.front.lock()?;
        let qid = state.qid_for(ROOT_ID)?;
        state.fids.insert(fid, ROOT_ID);
        Ok(qid)
    }

    fn walk(&mut self, fid: Fid, newfid: Fid, _start: Qid, names: &[Vec<u8>]) -> Result<Vec<Qid>> {
        let mut state = self.front.lock()?;
        let mut current = *state
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
                    Body::File(_) => None,
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
            state.fids.insert(newfid, current);
        }
        Ok(qids)
    }

    fn open(&mut self, fid: Fid, qid: Qid, mode: u8) -> Result<OpenFile> {
        if mode & 0x03 != 0 || mode & 0x10 != 0 {
            return Err(Error::from_static(EPERM));
        }
        let state = self.front.lock()?;
        let id = *state
            .fids
            .get(&fid)
            .ok_or_else(|| Error::from_static(EBADFID))?;
        if state.qid_for(id)?.path != qid.path {
            return Err(Error::from_static(EBADFID));
        }
        Ok(OpenFile { qid, iounit: 0 })
    }

    fn read(&mut self, fid: Fid, _qid: Qid, offset: u64, count: u32) -> Result<ReadData> {
        let state = self.front.lock()?;
        let id = *state
            .fids
            .get(&fid)
            .ok_or_else(|| Error::from_static(EBADFID))?;
        match &state.node(id)?.body {
            Body::Dir(children) => {
                if offset != 0 {
                    return Ok(ReadData::Directory(Vec::new()));
                }
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
        }
    }

    fn stat(&mut self, qid: Qid) -> Result<Stat> {
        self.front.lock()?.stat_for(qid.path)
    }

    fn clunk(&mut self, fid: Fid, _qid: Qid) -> Result<()> {
        self.front.lock()?.fids.remove(&fid);
        Ok(())
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
