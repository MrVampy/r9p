use crate::{
    error::{Error, Result},
    p9::{Client, OREAD},
};
use r9p::{
    fid::Fid,
    qid::{Qid, DMDIR, DMSYMLINK, QTDIR, QTSYMLINK},
    stat::Stat,
};
use std::{collections::BTreeMap, fmt, time::Duration};

pub const ROOT_NODEID: u64 = 1;

#[derive(Debug, Clone)]
pub struct Node {
    pub fid: Option<Fid>,
    pub path: Vec<Vec<u8>>,
    pub qid: Qid,
    pub stat: Stat,
    pub generation: u64,
    pub lookups: u64,
    pub needs_rebind: bool,
}

#[derive(Clone)]
pub struct Handle {
    pub client: Client,
    pub fid: Fid,
    pub is_dir: bool,
    pub write_on_release: bool,
    pub dir_entries: Vec<DirEntry>,
}

impl fmt::Debug for Handle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Handle")
            .field("fid", &self.fid)
            .field("is_dir", &self.is_dir)
            .field("write_on_release", &self.write_on_release)
            .field("dir_entries", &self.dir_entries)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: Vec<u8>,
    pub qid: Qid,
    pub stat: Stat,
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
        let old_fid = node.fid;
        node.fid = Some(fid);
        node.qid = stat.qid;
        node.stat = stat;
        node.generation = node.generation.saturating_add(1).max(1);
        node.needs_rebind = false;
        Ok(old_fid.filter(|old| *old != fid))
    }

    pub fn update_stat(&mut self, nodeid: u64, stat: Stat) -> Result<()> {
        let node = self.node_mut(nodeid)?;
        node.qid = stat.qid;
        node.stat = stat;
        node.needs_rebind = false;
        Ok(())
    }

    pub fn open_handle(
        &mut self,
        client: Client,
        fid: Fid,
        is_dir: bool,
        write_on_release: bool,
        dir_entries: Vec<DirEntry>,
    ) -> u64 {
        let handle = self.next_handle;
        self.next_handle = self.next_handle.saturating_add(1).max(1);
        self.handles.insert(
            handle,
            Handle {
                client,
                fid,
                is_dir,
                write_on_release,
                dir_entries,
            },
        );
        handle
    }

    pub fn handle(&self, handle: u64) -> Result<&Handle> {
        self.handles
            .get(&handle)
            .ok_or_else(|| Error::new(libc::ESTALE, format!("unknown file handle {handle}")))
    }

    pub fn remove_handle(&mut self, handle: u64) -> Option<Handle> {
        self.handles.remove(&handle)
    }

    pub fn refresh_qid(&mut self, qid: Qid, stat: Stat, path: Option<Vec<Vec<u8>>>) {
        for node in self.nodes.values_mut() {
            if same_qid(node.qid, qid) {
                if let Some(path) = &path {
                    node.path = path.clone();
                }
                node.qid = stat.qid;
                node.stat = stat.clone();
                node.generation = node.generation.saturating_add(1).max(1);
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
                let old_fid = node.fid;
                node.fid = Some(fid);
                if let Some(path) = path {
                    node.path = path;
                }
                node.qid = stat.qid;
                node.stat = stat;
                node.generation = node.generation.saturating_add(1).max(1);
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
            if let Some(node) = self.nodes.remove(&nodeid) {
                if let Some(fid) = node.fid {
                    replaced.push(fid);
                }
            }
        }
        for (nodeid, fid, stat) in rebound {
            if let Some(node) = self.nodes.get_mut(&nodeid) {
                if let Some(old_fid) = node.fid {
                    replaced.push(old_fid);
                }
                node.fid = Some(fid);
                node.qid = stat.qid;
                node.stat = stat;
                node.generation = node.generation.saturating_add(1).max(1);
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

    pub fn parent_entry(&self, path: &[Vec<u8>]) -> Option<(u64, Vec<u8>)> {
        let (name, parent) = path.split_last()?;
        self.nodeid_at_path(parent)
            .map(|parent_nodeid| (parent_nodeid, name.clone()))
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

pub fn is_dir(stat: &Stat) -> bool {
    stat.qid.qtype & QTDIR != 0 || stat.mode & DMDIR != 0
}

pub fn is_symlink(stat: &Stat) -> bool {
    stat.qid.qtype & QTSYMLINK != 0 || stat.mode & DMSYMLINK != 0
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

pub fn read_directory_entries(
    client: &mut Client,
    fid: Fid,
    timeout: Duration,
) -> Result<Vec<DirEntry>> {
    let dir_fid = client.clone_fid_timeout(fid, timeout)?;
    if let Err(error) = client.open_timeout(dir_fid, OREAD, timeout) {
        let _ = client.clunk_timeout(dir_fid, timeout);
        return Err(error);
    }
    let result = read_directory_entries_open(client, dir_fid, timeout);
    let _ = client.clunk_timeout(dir_fid, timeout);
    result
}

fn read_directory_entries_open(
    client: &mut Client,
    fid: Fid,
    timeout: Duration,
) -> Result<Vec<DirEntry>> {
    let mut offset = 0_u64;
    let mut all = Vec::new();
    loop {
        let chunk = client.read_timeout(fid, offset, 64 * 1024, timeout)?;
        if chunk.is_empty() {
            break;
        }
        offset = offset.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        all.extend(chunk);
    }
    decode_dir_entries(&all)
}

pub fn decode_dir_entries(data: &[u8]) -> Result<Vec<DirEntry>> {
    let mut entries = Vec::new();
    let mut offset = 0_usize;
    while offset < data.len() {
        if data.len().saturating_sub(offset) < 2 {
            return Err(Error::new(
                libc::EPROTO,
                "truncated 9P stat in directory read",
            ));
        }
        let size = u16::from_le_bytes([data[offset], data[offset + 1]]) as usize;
        let end = offset
            .checked_add(size + 2)
            .ok_or_else(|| Error::new(libc::EPROTO, "directory stat overflow"))?;
        let bytes = data
            .get(offset..end)
            .ok_or_else(|| Error::new(libc::EPROTO, "truncated 9P stat in directory read"))?;
        let stat = Stat::decode(bytes)
            .map_err(|error| Error::new(libc::EPROTO, format!("decode dir stat: {error}")))?;
        entries.push(DirEntry {
            name: stat.name.clone(),
            qid: stat.qid,
            stat,
        });
        offset = end;
    }
    Ok(entries)
}

pub fn null_wstat() -> Stat {
    Stat {
        type_: u16::MAX,
        dev: u32::MAX,
        qid: Qid::new(u8::MAX, u32::MAX, u64::MAX),
        mode: u32::MAX,
        atime: u32::MAX,
        mtime: u32::MAX,
        length: u64::MAX,
        name: Vec::new(),
        uid: Vec::new(),
        gid: Vec::new(),
        muid: Vec::new(),
    }
}

fn same_qid(a: Qid, b: Qid) -> bool {
    a.path == b.path && a.version == b.version && a.qtype == b.qtype
}

fn path_has_prefix(path: &[Vec<u8>], prefix: &[Vec<u8>]) -> bool {
    path.starts_with(prefix)
}

#[cfg(test)]
mod tests {
    use super::{mode_kind, qid_to_inode, NodeTable, ROOT_NODEID};
    use r9p::qid::{Qid, DMSYMLINK};
    use r9p::stat::Stat;

    #[test]
    fn inode_stays_under_signed_stat_boundary() {
        let inode = qid_to_inode(Qid::new(0x80, 0, u64::MAX));
        assert!(inode < (1_u64 << 63));
    }

    #[test]
    fn symlink_stats_map_to_fuse_symlink_mode() {
        let stat = Stat::new(
            "link",
            Qid::new(r9p::qid::QTSYMLINK, 0, 7),
            DMSYMLINK | 0o777,
        );

        assert_eq!(
            libc::S_IFLNK | 0o777,
            mode_kind(&stat) | (stat.mode & 0o777)
        );
    }

    #[test]
    fn lookup_nodes_remember_path_lineage() {
        let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
        let docs = nodes
            .insert_lookup(
                ROOT_NODEID,
                2,
                Stat::new("docs", Qid::dir(2), 0o555),
                b"docs",
            )
            .map(|inserted| inserted.nodeid)
            .expect("docs node should insert");
        let alpha = nodes
            .insert_lookup(
                docs,
                3,
                Stat::new("alpha.md", Qid::file(3), 0o444),
                b"alpha.md",
            )
            .map(|inserted| inserted.nodeid)
            .expect("alpha node should insert");

        assert_eq!(nodes.node(docs).expect("docs").path, vec![b"docs".to_vec()]);
        assert_eq!(
            nodes.node(alpha).expect("alpha").path,
            vec![b"docs".to_vec(), b"alpha.md".to_vec()]
        );
    }

    #[test]
    fn lazy_lookup_nodes_keep_stat_without_a_fid_until_bound() {
        let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
        let docs = nodes
            .insert_lookup_lazy(ROOT_NODEID, Stat::new("docs", Qid::dir(2), 0o555), b"docs")
            .expect("docs node should insert");

        let lazy = nodes.node(docs).expect("docs");
        assert_eq!(lazy.fid, None);
        assert_eq!(lazy.path, vec![b"docs".to_vec()]);
        assert_eq!(lazy.stat.qid, Qid::dir(2));

        let replaced = nodes
            .replace_binding(docs, 2, Stat::new("docs", Qid::dir(3), 0o555))
            .expect("docs node should bind");

        assert_eq!(replaced, None);
        let bound = nodes.node(docs).expect("docs");
        assert_eq!(bound.fid, Some(2));
        assert_eq!(bound.stat.qid, Qid::dir(3));
    }

    #[test]
    fn replacing_binding_returns_superseded_fid() {
        let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
        let docs = nodes
            .insert_lookup(
                ROOT_NODEID,
                2,
                Stat::new("docs", Qid::dir(2), 0o555),
                b"docs",
            )
            .map(|inserted| inserted.nodeid)
            .expect("docs node should insert");

        let replaced = nodes
            .replace_binding(docs, 3, Stat::new("docs", Qid::dir(3), 0o555))
            .expect("docs node should rebind");

        assert_eq!(replaced, Some(2));
        let rebound = nodes.node(docs).expect("docs");
        assert_eq!(rebound.fid, Some(3));
        assert_eq!(rebound.stat.qid, Qid::dir(3));
    }

    #[test]
    fn forgetting_lazy_nodes_has_no_fid_to_clunk() {
        let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
        let docs = nodes
            .insert_lookup_lazy(ROOT_NODEID, Stat::new("docs", Qid::dir(2), 0o555), b"docs")
            .expect("docs node should insert");

        assert_eq!(nodes.forget(docs, 1), None);
        assert!(nodes.node(docs).is_err());
    }

    #[test]
    fn forget_returns_removed_fid_without_clunking_under_lock() {
        let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
        let docs = nodes
            .insert_lookup(
                ROOT_NODEID,
                2,
                Stat::new("docs", Qid::dir(2), 0o555),
                b"docs",
            )
            .map(|inserted| inserted.nodeid)
            .expect("docs node should insert");

        assert_eq!(nodes.forget(docs, 1), Some(2));
        assert!(nodes.node(docs).is_err());
    }

    #[test]
    fn lookup_reuses_path_and_discards_duplicate_fid() {
        let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
        let first = nodes
            .insert_lookup(
                ROOT_NODEID,
                2,
                Stat::new("docs", Qid::dir(2), 0o555),
                b"docs",
            )
            .expect("first docs lookup should insert");
        let second = nodes
            .insert_lookup(
                ROOT_NODEID,
                3,
                Stat::new("docs", Qid::dir(2), 0o555),
                b"docs",
            )
            .expect("second docs lookup should reuse path");

        assert_eq!(second.nodeid, first.nodeid);
        assert_eq!(second.clunk_fid, Some(3));
        let docs = nodes.node(second.nodeid).expect("docs");
        assert_eq!(docs.fid, Some(2));
        assert_eq!(docs.lookups, 2);
    }

    #[test]
    fn remove_path_subtree_drops_cached_descendants() {
        let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
        let docs = nodes
            .insert_lookup(
                ROOT_NODEID,
                2,
                Stat::new("docs", Qid::dir(2), 0o555),
                b"docs",
            )
            .map(|inserted| inserted.nodeid)
            .expect("docs node should insert");
        let alpha = nodes
            .insert_lookup(
                docs,
                3,
                Stat::new("alpha.md", Qid::file(3), 0o444),
                b"alpha.md",
            )
            .map(|inserted| inserted.nodeid)
            .expect("alpha node should insert");

        let stale = nodes.remove_path_subtree(&[b"docs".to_vec()]);

        assert_eq!(stale, vec![2, 3]);
        assert!(nodes.node(docs).is_err());
        assert!(nodes.node(alpha).is_err());
        assert!(nodes.node(ROOT_NODEID).is_ok());
    }

    #[test]
    fn move_path_prefix_moves_cached_descendants() {
        let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
        let docs = nodes
            .insert_lookup(
                ROOT_NODEID,
                2,
                Stat::new("docs", Qid::dir(2), 0o555),
                b"docs",
            )
            .map(|inserted| inserted.nodeid)
            .expect("docs node should insert");
        let alpha = nodes
            .insert_lookup(
                docs,
                3,
                Stat::new("alpha.md", Qid::file(3), 0o444),
                b"alpha.md",
            )
            .map(|inserted| inserted.nodeid)
            .expect("alpha node should insert");

        nodes.move_path_prefix(&[b"docs".to_vec()], &[b"notes".to_vec()]);

        assert_eq!(
            nodes.node(docs).expect("docs").path,
            vec![b"notes".to_vec()]
        );
        assert_eq!(
            nodes.node(alpha).expect("alpha").path,
            vec![b"notes".to_vec(), b"alpha.md".to_vec()]
        );
    }

    #[test]
    fn rebind_paths_snapshots_paths_without_network_work() {
        let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
        let docs = nodes
            .insert_lookup(
                ROOT_NODEID,
                2,
                Stat::new("docs", Qid::dir(2), 0o555),
                b"docs",
            )
            .map(|inserted| inserted.nodeid)
            .expect("docs node should insert");
        let alpha = nodes
            .insert_lookup(
                docs,
                3,
                Stat::new("alpha.md", Qid::file(3), 0o444),
                b"alpha.md",
            )
            .map(|inserted| inserted.nodeid)
            .expect("alpha node should insert");

        let paths = nodes.rebind_paths();

        assert_eq!(
            paths,
            vec![
                (ROOT_NODEID, vec![]),
                (docs, vec![b"docs".to_vec()]),
                (alpha, vec![b"docs".to_vec(), b"alpha.md".to_vec()]),
            ]
        );
    }

    #[test]
    fn apply_rebind_results_updates_fresh_nodes_and_drops_stale_nodes() {
        let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
        let docs = nodes
            .insert_lookup(
                ROOT_NODEID,
                2,
                Stat::new("docs", Qid::dir(2), 0o555),
                b"docs",
            )
            .map(|inserted| inserted.nodeid)
            .expect("docs node should insert");
        let stale = nodes
            .insert_lookup(
                docs,
                3,
                Stat::new("stale.md", Qid::file(3), 0o444),
                b"stale.md",
            )
            .map(|inserted| inserted.nodeid)
            .expect("stale node should insert");

        let replaced = nodes.apply_rebind_results(
            vec![
                (ROOT_NODEID, 10, Stat::new("", Qid::dir(10), 0o555)),
                (docs, 11, Stat::new("docs", Qid::dir(11), 0o555)),
            ],
            vec![stale],
        );

        assert_eq!(replaced, vec![3, 1, 2]);
        assert_eq!(nodes.node(ROOT_NODEID).expect("root").fid, Some(10));
        assert_eq!(nodes.node(ROOT_NODEID).expect("root").qid, Qid::dir(10));
        assert_eq!(nodes.node(docs).expect("docs").fid, Some(11));
        assert_eq!(nodes.node(docs).expect("docs").qid, Qid::dir(11));
        assert!(nodes.node(stale).is_err());
    }

    #[test]
    fn targeted_stale_marking_leaves_unrelated_nodes_fresh() {
        let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
        let a = nodes
            .insert_lookup(ROOT_NODEID, 2, Stat::new("a", Qid::dir(2), 0o555), b"a")
            .map(|inserted| inserted.nodeid)
            .expect("a node should insert");
        let b = nodes
            .insert_lookup(ROOT_NODEID, 3, Stat::new("b", Qid::dir(3), 0o555), b"b")
            .map(|inserted| inserted.nodeid)
            .expect("b node should insert");
        let child = nodes
            .insert_lookup(a, 4, Stat::new("child", Qid::file(4), 0o444), b"child")
            .map(|inserted| inserted.nodeid)
            .expect("child node should insert");

        let stale = nodes.mark_path_prefix_stale(&[b"a".to_vec()]);

        assert_eq!(stale.len(), 2);
        assert!(nodes.node(a).expect("a").needs_rebind);
        assert!(nodes.node(child).expect("child").needs_rebind);
        assert!(!nodes.node(b).expect("b").needs_rebind);
        assert_eq!(
            nodes.parent_entry(&[b"a".to_vec(), b"child".to_vec()]),
            Some((a, b"child".to_vec()))
        );
    }

    #[test]
    fn forget_decrements_lookup_count_before_removing() {
        let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
        let docs = nodes
            .insert_lookup(
                ROOT_NODEID,
                2,
                Stat::new("docs", Qid::dir(2), 0o555),
                b"docs",
            )
            .map(|inserted| inserted.nodeid)
            .expect("docs node should insert");
        nodes.node_mut(docs).expect("docs").lookups = 2;

        assert_eq!(nodes.forget(docs, 1), None);
        assert_eq!(nodes.node(docs).expect("docs").lookups, 1);
        assert_eq!(nodes.forget(docs, 1), Some(2));
    }

    #[test]
    fn namespace_mutation_marks_path_bindings_stale_without_network_rebind() {
        let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
        let docs = nodes
            .insert_lookup(
                ROOT_NODEID,
                2,
                Stat::new("docs", Qid::dir(2), 0o555),
                b"docs",
            )
            .map(|inserted| inserted.nodeid)
            .expect("docs node should insert");
        let alpha = nodes
            .insert_lookup(
                docs,
                3,
                Stat::new("alpha.md", Qid::file(3), 0o444),
                b"alpha.md",
            )
            .map(|inserted| inserted.nodeid)
            .expect("alpha node should insert");

        let stale = nodes.mark_path_bindings_stale();

        assert_eq!(
            stale.iter().map(|binding| binding.fid).collect::<Vec<_>>(),
            vec![Some(2), Some(3)]
        );
        assert_eq!(stale[0].nodeid, docs);
        assert_eq!(stale[0].parent_nodeid, Some(ROOT_NODEID));
        assert_eq!(stale[0].name, b"docs".to_vec());
        assert_eq!(stale[1].nodeid, alpha);
        assert_eq!(stale[1].parent_nodeid, Some(docs));
        assert_eq!(stale[1].name, b"alpha.md".to_vec());
        let root = nodes.node(ROOT_NODEID).expect("root");
        assert_eq!(root.fid, Some(1));
        assert!(!root.needs_rebind);
        let docs_node = nodes.node(docs).expect("docs");
        assert_eq!(docs_node.fid, None);
        assert!(docs_node.needs_rebind);
        let alpha_node = nodes.node(alpha).expect("alpha");
        assert_eq!(alpha_node.fid, None);
        assert!(alpha_node.needs_rebind);
    }
}
