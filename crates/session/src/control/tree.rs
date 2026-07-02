use super::{freshness::ResponseFreshness, json, query, snapshot, status_json, ControlConfig};
use crate::{feed::FeedState, Client, ORDWR, OREAD};
use r9p::{
    error::{Error as P9Error, EEXIST, EPERM},
    fid::Fid,
    qid::{Qid, DMDIR},
    server::{FileTree, OpenFile, ReadData},
    stat::Stat,
};
use std::collections::BTreeMap;

const USAGE: &str = concat!(
    "r9p session control namespace\n",
    "\n",
    "files:\n",
    "  status             session attachment status JSON\n",
    "  usage              this text\n",
    "  query              JSON RPC file: write {\"op\":\"stat\",\"path\":\"/\"}, read response\n",
    "  stat/<path>        stat JSON for namespace path; use stat/. for root\n",
    "  list/<path>        directory listing JSON; use list/. for root\n",
    "  read/<path>        file content report JSON; use read/. for root\n",
    "  snapshot/<depth>/<path> subtree snapshot JSON; use snapshot/<depth>/. for root\n",
);

#[derive(Clone)]
pub(super) struct ControlTree {
    client: Client,
    config: ControlConfig,
    feed_state: FeedState,
    session_epoch: String,
    nodes: BTreeMap<u64, ControlNode>,
    qids: BTreeMap<ControlNode, u64>,
    query_responses: BTreeMap<Fid, Vec<u8>>,
    next_qid: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ControlNode {
    Root,
    Usage,
    Status,
    Query,
    StatRoot,
    Stat(Vec<String>),
    ListRoot,
    List(Vec<String>),
    ReadRoot,
    Read(Vec<String>),
    SnapshotRoot,
    SnapshotDepth(usize),
    Snapshot { depth: usize, path: Vec<String> },
}

impl ControlTree {
    pub(super) fn new(
        client: Client,
        config: ControlConfig,
        feed_state: FeedState,
        session_epoch: String,
    ) -> Self {
        let mut tree = Self {
            client,
            config,
            feed_state,
            session_epoch,
            nodes: BTreeMap::new(),
            qids: BTreeMap::new(),
            query_responses: BTreeMap::new(),
            next_qid: 1,
        };
        tree.qid_for(ControlNode::Root);
        tree
    }

    fn qid_for(&mut self, node: ControlNode) -> Qid {
        if let Some(path) = self.qids.get(&node) {
            return qid_for_path(*path, &node);
        }
        let path = self.next_qid;
        self.next_qid = self.next_qid.saturating_add(1);
        self.nodes.insert(path, node.clone());
        self.qids.insert(node.clone(), path);
        qid_for_path(path, &node)
    }

    fn node_for(&self, qid: Qid) -> r9p::Result<ControlNode> {
        self.nodes
            .get(&qid.path)
            .cloned()
            .ok_or_else(|| P9Error::from_static(EEXIST))
    }

    fn stat_for(&mut self, node: ControlNode) -> Stat {
        let qid = self.qid_for(node.clone());
        stat_for_node(qid, &node)
    }

    fn root_entries(&mut self) -> Vec<Stat> {
        vec![
            self.stat_for(ControlNode::Status),
            self.stat_for(ControlNode::Usage),
            self.stat_for(ControlNode::Query),
            self.stat_for(ControlNode::StatRoot),
            self.stat_for(ControlNode::ListRoot),
            self.stat_for(ControlNode::ReadRoot),
            self.stat_for(ControlNode::SnapshotRoot),
        ]
    }
}

impl FileTree for ControlTree {
    fn attach(&mut self, _fid: Fid, _uname: &[u8], _aname: &[u8]) -> r9p::Result<Qid> {
        Ok(self.qid_for(ControlNode::Root))
    }

    fn walk(
        &mut self,
        _fid: Fid,
        _newfid: Fid,
        start: Qid,
        names: &[Vec<u8>],
    ) -> r9p::Result<Vec<Qid>> {
        let mut current = self.node_for(start)?;
        let mut qids = Vec::with_capacity(names.len());
        for name in names {
            let Some(next) = walk_child(&current, name) else {
                break;
            };
            let qid = self.qid_for(next.clone());
            qids.push(qid);
            current = next;
        }
        Ok(qids)
    }

    fn open(&mut self, _fid: Fid, qid: Qid, mode: u8) -> r9p::Result<OpenFile> {
        let node = self.node_for(qid)?;
        let access = mode & 0x3;
        if matches!(node, ControlNode::Query) {
            if access != OREAD && access != ORDWR {
                return Err(P9Error::from_static(EPERM));
            }
        } else if access != OREAD {
            return Err(P9Error::from_static(EPERM));
        }
        Ok(OpenFile { qid, iounit: 0 })
    }

    fn read(&mut self, fid: Fid, qid: Qid, offset: u64, count: u32) -> r9p::Result<ReadData> {
        match self.node_for(qid)? {
            ControlNode::Root => Ok(ReadData::Directory(self.root_entries())),
            ControlNode::StatRoot
            | ControlNode::ListRoot
            | ControlNode::ReadRoot
            | ControlNode::SnapshotRoot
            | ControlNode::SnapshotDepth(_) => Ok(ReadData::Directory(Vec::new())),
            ControlNode::Usage => read_bytes(USAGE.as_bytes(), offset, count),
            ControlNode::Status => {
                let response = status_json(
                    &self.client,
                    &self.config,
                    &self.feed_state,
                    &self.session_epoch,
                )
                .map_err(p9_error)?;
                read_bytes(response.as_bytes(), offset, count)
            }
            ControlNode::Query => {
                let response = self.query_responses.get(&fid).cloned().unwrap_or_else(|| {
                    json::error_response("query_required", "write query JSON first").into_bytes()
                });
                read_bytes(&response, offset, count)
            }
            ControlNode::Stat(path) => {
                let response = snapshot::stat_json(
                    &self.client,
                    &namespace_path(&path),
                    self.config.request_timeout,
                    &self.response_freshness(),
                )
                .map_err(p9_error)?;
                read_bytes(response.as_bytes(), offset, count)
            }
            ControlNode::List(path) => {
                let response = snapshot::list_json(
                    &self.client,
                    &namespace_path(&path),
                    self.config.request_timeout,
                    &self.response_freshness(),
                )
                .map_err(p9_error)?;
                read_bytes(response.as_bytes(), offset, count)
            }
            ControlNode::Read(path) => {
                let response = snapshot::read_json(
                    &self.client,
                    &namespace_path(&path),
                    self.config.request_timeout,
                    &self.response_freshness(),
                )
                .map_err(p9_error)?;
                read_bytes(response.as_bytes(), offset, count)
            }
            ControlNode::Snapshot { depth, path } => {
                let response = snapshot::snapshot_json(
                    &self.client,
                    &namespace_path(&path),
                    depth,
                    self.config.request_timeout,
                    &self.response_freshness(),
                )
                .map_err(p9_error)?;
                read_bytes(response.as_bytes(), offset, count)
            }
        }
    }

    fn stat(&mut self, qid: Qid) -> r9p::Result<Stat> {
        let node = self.node_for(qid)?;
        Ok(stat_for_node(qid_for_path(qid.path, &node), &node))
    }

    fn write(&mut self, fid: Fid, qid: Qid, offset: u64, data: &[u8]) -> r9p::Result<u32> {
        if offset != 0 {
            return Err(P9Error::from("query writes must start at offset 0"));
        }
        match self.node_for(qid)? {
            ControlNode::Query => {
                let response = match query::parse_json(data) {
                    Ok(request) => query::response_json(
                        &self.client,
                        &self.config,
                        &self.feed_state,
                        &self.session_epoch,
                        request,
                    ),
                    Err(error) => json::error_response("bad_query", &error),
                };
                self.query_responses.insert(fid, response.into_bytes());
                u32::try_from(data.len()).map_err(|_| P9Error::from("query write too large"))
            }
            _ => Err(P9Error::from_static(EPERM)),
        }
    }

    fn clunk(&mut self, fid: Fid, _qid: Qid) -> r9p::Result<()> {
        self.query_responses.remove(&fid);
        Ok(())
    }
}

impl ControlTree {
    fn response_freshness(&self) -> ResponseFreshness {
        ResponseFreshness::from_feed(&self.session_epoch, &self.feed_state)
    }
}

fn walk_child(node: &ControlNode, name: &[u8]) -> Option<ControlNode> {
    let name = String::from_utf8_lossy(name).to_string();
    match node {
        ControlNode::Root => match name.as_str() {
            "usage" => Some(ControlNode::Usage),
            "status" => Some(ControlNode::Status),
            "query" => Some(ControlNode::Query),
            "stat" => Some(ControlNode::StatRoot),
            "list" => Some(ControlNode::ListRoot),
            "read" => Some(ControlNode::ReadRoot),
            "snapshot" => Some(ControlNode::SnapshotRoot),
            _ => None,
        },
        ControlNode::StatRoot => Some(ControlNode::Stat(extend_path(Vec::new(), name))),
        ControlNode::Stat(path) => Some(ControlNode::Stat(extend_path(path.clone(), name))),
        ControlNode::ListRoot => Some(ControlNode::List(extend_path(Vec::new(), name))),
        ControlNode::List(path) => Some(ControlNode::List(extend_path(path.clone(), name))),
        ControlNode::ReadRoot => Some(ControlNode::Read(extend_path(Vec::new(), name))),
        ControlNode::Read(path) => Some(ControlNode::Read(extend_path(path.clone(), name))),
        ControlNode::SnapshotRoot => name.parse::<usize>().ok().map(ControlNode::SnapshotDepth),
        ControlNode::SnapshotDepth(depth) => Some(ControlNode::Snapshot {
            depth: *depth,
            path: extend_path(Vec::new(), name),
        }),
        ControlNode::Snapshot { depth, path } => Some(ControlNode::Snapshot {
            depth: *depth,
            path: extend_path(path.clone(), name),
        }),
        ControlNode::Usage | ControlNode::Status | ControlNode::Query => None,
    }
}

fn extend_path(mut path: Vec<String>, name: String) -> Vec<String> {
    if path.is_empty() && name == "." {
        return path;
    }
    path.push(name);
    path
}

fn namespace_path(path: &[String]) -> String {
    if path.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", path.join("/"))
    }
}

fn qid_for_path(path: u64, node: &ControlNode) -> Qid {
    if is_dir_node(node) {
        Qid::dir(path)
    } else {
        Qid::file(path)
    }
}

fn stat_for_node(qid: Qid, node: &ControlNode) -> Stat {
    let mode = if is_dir_node(node) {
        DMDIR | 0o555
    } else {
        0o444
    };
    Stat::new(node_name(node), qid, mode)
}

fn node_name(node: &ControlNode) -> Vec<u8> {
    match node {
        ControlNode::Root => b".".to_vec(),
        ControlNode::Usage => b"usage".to_vec(),
        ControlNode::Status => b"status".to_vec(),
        ControlNode::Query => b"query".to_vec(),
        ControlNode::StatRoot => b"stat".to_vec(),
        ControlNode::Stat(path) => query_name(path),
        ControlNode::ListRoot => b"list".to_vec(),
        ControlNode::List(path) => query_name(path),
        ControlNode::ReadRoot => b"read".to_vec(),
        ControlNode::Read(path) => query_name(path),
        ControlNode::SnapshotRoot => b"snapshot".to_vec(),
        ControlNode::SnapshotDepth(depth) => depth.to_string().into_bytes(),
        ControlNode::Snapshot { path, .. } => query_name(path),
    }
}

fn query_name(path: &[String]) -> Vec<u8> {
    path.last()
        .map(|name| name.as_bytes().to_vec())
        .unwrap_or_else(|| b".".to_vec())
}

fn is_dir_node(node: &ControlNode) -> bool {
    matches!(
        node,
        ControlNode::Root
            | ControlNode::StatRoot
            | ControlNode::ListRoot
            | ControlNode::ReadRoot
            | ControlNode::SnapshotRoot
            | ControlNode::SnapshotDepth(_)
    )
}

fn read_bytes(bytes: &[u8], offset: u64, count: u32) -> r9p::Result<ReadData> {
    let start = usize::try_from(offset)
        .map_err(|_| P9Error::from("read offset too large"))?
        .min(bytes.len());
    let end = start
        .saturating_add(usize::try_from(count).unwrap_or(usize::MAX))
        .min(bytes.len());
    Ok(ReadData::Bytes(bytes[start..end].to_vec()))
}

fn p9_error(error: crate::Error) -> P9Error {
    P9Error::from(error.message().to_string())
}

#[cfg(test)]
mod tests {
    use super::{namespace_path, walk_child, ControlNode};

    #[test]
    fn dot_names_the_root_query_path() {
        let node = walk_child(&ControlNode::StatRoot, b".").expect("dot should walk");
        assert_eq!(node, ControlNode::Stat(Vec::new()));
    }

    #[test]
    fn nested_paths_keep_namespace_segments() {
        let node = walk_child(&ControlNode::ListRoot, b"srv").expect("first segment");
        let node = walk_child(&node, b"runtime").expect("second segment");
        assert_eq!(
            node,
            ControlNode::List(vec!["srv".to_string(), "runtime".to_string()])
        );
    }

    #[test]
    fn namespace_path_formats_root_and_nested_paths() {
        assert_eq!(namespace_path(&[]), "/");
        assert_eq!(
            namespace_path(&["srv".to_string(), "runtime".to_string()]),
            "/srv/runtime"
        );
    }
}
