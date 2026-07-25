use crate::error::{Error, Result};
use r9p::{fid::Fid, qid::Qid, stat::Stat};
pub use session::{is_dir, is_symlink, read_open_directory_entries, DirCache, DirEntry};
use session::{same_qid, Freshness, StaleReason};
use std::collections::BTreeMap;

mod handles;
pub use handles::Handle;
pub(crate) use handles::OpenHandle;

pub const ROOT_NODEID: u64 = 1;
pub const CLOSE_COMMIT_MODE_FLAG: u32 = 0x0100_0000;

pub fn has_close_commit_mode(stat: &Stat) -> bool {
    stat.mode & CLOSE_COMMIT_MODE_FLAG != 0
}

#[derive(Debug, Clone)]
pub struct Node {
    pub fid: Option<Fid>,
    pub path: Vec<Vec<u8>>,
    pub qid: Qid,
    pub stat: Stat,
    pub stat_freshness: Freshness,
    pub dir_cache: Option<DirCache>,
    pub generation: u64,
    pub lookups: u64,
    pub needs_rebind: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InsertedNode {
    pub nodeid: u64,
    pub clunk_fid: Option<Fid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleBinding {
    pub nodeid: u64,
    pub parent_nodeid: Option<u64>,
    pub name: Vec<u8>,
    pub fid: Option<Fid>,
}

#[derive(Debug)]
pub struct NodeTable {
    nodes: BTreeMap<u64, Node>,
    handles: BTreeMap<u64, Handle>,
    next_nodeid: u64,
    next_handle: u64,
}

impl NodeTable {
    pub fn new(root_fid: Fid, root_stat: Stat) -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert(
            ROOT_NODEID,
            Node {
                fid: Some(root_fid),
                path: Vec::new(),
                qid: root_stat.qid,
                stat: root_stat,
                stat_freshness: Freshness::fresh_now(),
                dir_cache: None,
                generation: 1,
                lookups: 1,
                needs_rebind: false,
            },
        );
        Self {
            nodes,
            handles: BTreeMap::new(),
            next_nodeid: ROOT_NODEID + 1,
            next_handle: 1,
        }
    }

    pub fn node(&self, nodeid: u64) -> Result<&Node> {
        self.nodes
            .get(&nodeid)
            .ok_or_else(|| Error::new(libc::ESTALE, format!("unknown nodeid {nodeid}")))
    }

    pub fn node_mut(&mut self, nodeid: u64) -> Result<&mut Node> {
        self.nodes
            .get_mut(&nodeid)
            .ok_or_else(|| Error::new(libc::ESTALE, format!("unknown nodeid {nodeid}")))
    }

    pub fn insert_lookup(
        &mut self,
        parent_nodeid: u64,
        fid: Fid,
        stat: Stat,
        name: &[u8],
    ) -> Result<InsertedNode> {
        self.insert_node(parent_nodeid, Some(fid), stat, name)
    }

    pub fn insert_lookup_lazy(
        &mut self,
        parent_nodeid: u64,
        stat: Stat,
        name: &[u8],
    ) -> Result<u64> {
        self.insert_node(parent_nodeid, None, stat, name)
            .map(|inserted| inserted.nodeid)
    }

    fn insert_node(
        &mut self,
        parent_nodeid: u64,
        fid: Option<Fid>,
        stat: Stat,
        name: &[u8],
    ) -> Result<InsertedNode> {
        let mut path = self.node(parent_nodeid)?.path.clone();
        path.push(name.to_vec());
        if let Some(nodeid) = self.nodeid_at_path(&path) {
            let node = self
                .nodes
                .get_mut(&nodeid)
                .ok_or_else(|| Error::new(libc::ESTALE, format!("unknown nodeid {nodeid}")))?;
            let qid_changed = !same_qid(node.qid, stat.qid);
            let clunk_fid = match (fid, node.fid, qid_changed) {
                (Some(new_fid), Some(_old_fid), false) => Some(new_fid),
                (Some(new_fid), Some(old_fid), true) => {
                    node.fid = Some(new_fid);
                    (old_fid != new_fid).then_some(old_fid)
                }
                (Some(new_fid), None, _) => {
                    node.fid = Some(new_fid);
                    None
                }
                (None, _, _) => None,
            };
            node.qid = stat.qid;
            node.stat = stat;
            node.stat_freshness.mark_fresh();
            if qid_changed || !is_dir(&node.stat) {
                node.dir_cache = None;
            }
            if qid_changed {
                node.generation = node.generation.saturating_add(1).max(1);
            }
            node.lookups = node.lookups.saturating_add(1).max(1);
            node.needs_rebind = false;
            return Ok(InsertedNode { nodeid, clunk_fid });
        }
        let nodeid = self.next_nodeid;
        self.next_nodeid = self.next_nodeid.saturating_add(1).max(ROOT_NODEID + 1);
        self.nodes.insert(
            nodeid,
            Node {
                fid,
                path,
                qid: stat.qid,
                stat,
                stat_freshness: Freshness::fresh_now(),
                dir_cache: None,
                generation: 1,
                lookups: 1,
                needs_rebind: false,
            },
        );
        Ok(InsertedNode {
            nodeid,
            clunk_fid: None,
        })
    }

    pub fn forget(&mut self, nodeid: u64, count: u64) -> Option<Fid> {
        if nodeid == ROOT_NODEID {
            return None;
        }
        let remove = if let Some(node) = self.nodes.get_mut(&nodeid) {
            if node.lookups > count {
                node.lookups -= count;
                false
            } else {
                true
            }
        } else {
            false
        };
        if remove {
            return self.nodes.remove(&nodeid).and_then(|node| node.fid);
        }
        None
    }

    pub fn replace_binding(&mut self, nodeid: u64, fid: Fid, stat: Stat) -> Result<Option<Fid>> {
        let node = self.node_mut(nodeid)?;
        let identity_changed = !same_inode_identity(node, &stat);
        let old_fid = node.fid;
        node.fid = Some(fid);
        node.qid = stat.qid;
        node.stat = stat;
        node.stat_freshness.mark_fresh();
        if identity_changed || !is_dir(&node.stat) {
            node.dir_cache = None;
        }
        if identity_changed {
            node.generation = node.generation.saturating_add(1).max(1);
        }
        node.needs_rebind = false;
        Ok(old_fid.filter(|old| *old != fid))
    }

    pub fn take_cached_fid(&mut self, nodeid: u64) -> Result<Option<Fid>> {
        Ok(self.node_mut(nodeid)?.fid.take())
    }

    pub fn update_stat(&mut self, nodeid: u64, stat: Stat) -> Result<()> {
        let node = self.node_mut(nodeid)?;
        let identity_changed = !same_inode_identity(node, &stat);
        node.qid = stat.qid;
        node.stat = stat;
        node.stat_freshness.mark_fresh();
        if identity_changed || !is_dir(&node.stat) {
            node.dir_cache = None;
        }
        node.needs_rebind = false;
        Ok(())
    }

    pub fn update_dir_cache(&mut self, nodeid: u64, entries: Vec<DirEntry>) -> Result<()> {
        let node = self.node_mut(nodeid)?;
        if !is_dir(&node.stat) {
            return Err(Error::new(libc::ENOTDIR, "node is not a directory"));
        }
        node.dir_cache = Some(DirCache::fresh(entries));
        Ok(())
    }

    pub fn refresh_qid(&mut self, qid: Qid, stat: Stat, path: Option<Vec<Vec<u8>>>) {
        for node in self.nodes.values_mut() {
            if same_qid(node.qid, qid) {
                let identity_changed = !same_inode_identity(node, &stat);
                if let Some(path) = &path {
                    node.path = path.clone();
                }
                node.qid = stat.qid;
                node.stat = stat.clone();
                node.stat_freshness.mark_fresh();
                if identity_changed || !is_dir(&node.stat) {
                    node.dir_cache = None;
                }
                if identity_changed {
                    node.generation = node.generation.saturating_add(1).max(1);
                }
                node.needs_rebind = false;
            }
        }
    }

    pub fn replace_first_qid(
        &mut self,
        qid: Qid,
        fid: Fid,
        stat: Stat,
        path: Option<Vec<Vec<u8>>>,
    ) -> Option<Fid> {
        for node in self.nodes.values_mut() {
            if same_qid(node.qid, qid) {
                let identity_changed = !same_inode_identity(node, &stat);
                let old_fid = node.fid;
                node.fid = Some(fid);
                if let Some(path) = path {
                    node.path = path;
                }
                node.qid = stat.qid;
                node.stat = stat;
                node.stat_freshness.mark_fresh();
                if identity_changed || !is_dir(&node.stat) {
                    node.dir_cache = None;
                }
                if identity_changed {
                    node.generation = node.generation.saturating_add(1).max(1);
                }
                node.needs_rebind = false;
                return old_fid;
            }
        }
        None
    }

    pub fn child_path(&self, parent_nodeid: u64, name: &[u8]) -> Result<Vec<Vec<u8>>> {
        let mut path = self.node(parent_nodeid)?.path.clone();
        path.push(name.to_vec());
        Ok(path)
    }

    pub fn remove_path_subtree(&mut self, path: &[Vec<u8>]) -> Vec<Fid> {
        if path.is_empty() {
            return Vec::new();
        }
        let nodeids = self
            .nodes
            .iter()
            .filter_map(|(nodeid, node)| path_has_prefix(&node.path, path).then_some(*nodeid))
            .collect::<Vec<_>>();
        let mut fids = Vec::new();
        for nodeid in nodeids {
            if let Some(node) = self.nodes.remove(&nodeid) {
                if let Some(fid) = node.fid {
                    fids.push(fid);
                }
            }
        }
        fids
    }

    pub fn move_path_prefix(&mut self, from: &[Vec<u8>], to: &[Vec<u8>]) {
        if from.is_empty() {
            return;
        }
        for node in self.nodes.values_mut() {
            if path_has_prefix(&node.path, from) {
                let mut moved = to.to_vec();
                moved.extend_from_slice(&node.path[from.len()..]);
                node.path = moved;
            }
        }
    }

    fn nodeid_at_path(&self, path: &[Vec<u8>]) -> Option<u64> {
        self.nodes
            .iter()
            .find_map(|(nodeid, node)| (node.path == path).then_some(*nodeid))
    }

    pub fn parent_nodeid(&self, nodeid: u64) -> Result<u64> {
        let path = self.node(nodeid)?.path.clone();
        match path.split_last() {
            None => Ok(ROOT_NODEID),
            Some((_name, parent_path)) => self
                .nodeid_at_path(parent_path)
                .ok_or_else(|| Error::new(libc::ESTALE, format!("unknown parent for {nodeid}"))),
        }
    }

    pub fn rebind_paths(&self) -> Vec<(u64, Vec<Vec<u8>>)> {
        self.nodes
            .iter()
            .map(|(nodeid, node)| (*nodeid, node.path.clone()))
            .collect()
    }

    pub fn apply_rebind_results(
        &mut self,
        rebound: Vec<(u64, Fid, Stat)>,
        stale: Vec<u64>,
    ) -> Vec<Fid> {
        let mut replaced = Vec::new();
        for nodeid in stale {
            if let Some(node) = self.nodes.get_mut(&nodeid) {
                if let Some(fid) = node.fid.take() {
                    replaced.push(fid);
                }
                node.needs_rebind = true;
                node.stat_freshness.mark_stale(StaleReason::Reconnect);
                node.dir_cache = None;
            }
        }
        for (nodeid, fid, stat) in rebound {
            if let Some(node) = self.nodes.get_mut(&nodeid) {
                let identity_changed = !same_inode_identity(node, &stat);
                if let Some(old_fid) = node.fid {
                    replaced.push(old_fid);
                }
                node.fid = Some(fid);
                node.qid = stat.qid;
                node.stat = stat;
                node.stat_freshness.mark_fresh();
                if identity_changed || !is_dir(&node.stat) {
                    node.dir_cache = None;
                }
                if identity_changed {
                    node.generation = node.generation.saturating_add(1).max(1);
                }
                node.needs_rebind = false;
            }
        }
        replaced
    }

    pub fn mark_path_bindings_stale(&mut self) -> Vec<StaleBinding> {
        self.mark_path_prefix_stale(&[])
    }

    pub fn mark_path_stale(&mut self, path: &[Vec<u8>]) -> Vec<StaleBinding> {
        self.mark_path_with(path, false)
    }

    pub fn mark_path_prefix_stale(&mut self, path: &[Vec<u8>]) -> Vec<StaleBinding> {
        self.mark_path_with(path, true)
    }

    #[cfg(test)]
    pub fn parent_entry(&self, path: &[Vec<u8>]) -> Option<(u64, Vec<u8>)> {
        let (name, parent) = path.split_last()?;
        self.nodeid_at_path(parent)
            .map(|parent_nodeid| (parent_nodeid, name.clone()))
    }

    pub fn mark_parent_directory_cache_stale(
        &mut self,
        path: &[Vec<u8>],
    ) -> Option<(u64, Vec<u8>)> {
        let (name, parent) = path.split_last()?;
        let parent_nodeid = self.nodeid_at_path(parent)?;
        if let Some(parent_node) = self.nodes.get_mut(&parent_nodeid) {
            parent_node.dir_cache = None;
        }
        Some((parent_nodeid, name.clone()))
    }

    fn mark_path_with(&mut self, path: &[Vec<u8>], include_descendants: bool) -> Vec<StaleBinding> {
        let path_index = self
            .nodes
            .iter()
            .map(|(nodeid, node)| (node.path.clone(), *nodeid))
            .collect::<BTreeMap<_, _>>();
        let mut stale = Vec::new();
        for (nodeid, node) in self.nodes.iter_mut() {
            if *nodeid == ROOT_NODEID {
                continue;
            }
            let matches = if path.is_empty() {
                true
            } else if include_descendants {
                path_has_prefix(&node.path, path)
            } else {
                node.path == path
            };
            if !matches {
                continue;
            }
            let parent_nodeid = node
                .path
                .split_last()
                .and_then(|(_, parent)| path_index.get(parent).copied());
            let name = node
                .path
                .last()
                .cloned()
                .unwrap_or_else(|| node.stat.name.clone());
            let fid = node.fid.take();
            node.needs_rebind = true;
            node.stat_freshness.mark_stale(StaleReason::NamespaceChange);
            node.dir_cache = None;
            stale.push(StaleBinding {
                nodeid: *nodeid,
                parent_nodeid,
                name,
                fid,
            });
        }
        stale
    }
}

pub fn qid_to_inode(qid: Qid) -> u64 {
    (qid.path & ((1_u64 << 55) - 1)) | (u64::from(qid.qtype) << 55)
}

pub fn mode_kind(stat: &Stat) -> u32 {
    if is_dir(stat) {
        libc::S_IFDIR
    } else if is_symlink(stat) {
        libc::S_IFLNK
    } else {
        libc::S_IFREG
    }
}

fn same_inode_identity(node: &Node, stat: &Stat) -> bool {
    same_qid(node.qid, stat.qid) && mode_kind(&node.stat) == mode_kind(stat)
}

fn path_has_prefix(path: &[Vec<u8>], prefix: &[Vec<u8>]) -> bool {
    path.starts_with(prefix)
}

#[cfg(test)]
mod tests;
