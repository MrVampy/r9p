use super::{json, query, status_json, ControlConfig};
use crate::{feed::FeedState, ClientSlot, NamespaceCache, SessionEpoch, ORDWR, OREAD};
use r9p::{
    error::{Error as P9Error, ENOENT, EPERM},
    fid::Fid,
    mode::ACCESS_MASK,
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
    "  query              JSON RPC file for status, snapshot, stat, list, and read\n",
    "\n",
    "example:\n",
    "  write {\"op\":\"stat\",\"path\":\"/\"} to query, then read the same fid\n",
);

#[derive(Clone)]
pub(super) struct ControlTree {
    client: ClientSlot,
    config: ControlConfig,
    feed_state: FeedState,
    cache: NamespaceCache,
    session_epoch: SessionEpoch,
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
}

impl ControlTree {
    pub(super) fn new(
        client: ClientSlot,
        config: ControlConfig,
        feed_state: FeedState,
        cache: NamespaceCache,
        session_epoch: SessionEpoch,
    ) -> Self {
        let mut tree = Self {
            client,
            config,
            feed_state,
            cache,
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
            .ok_or_else(|| P9Error::from_static(ENOENT))
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
        ]
    }
}

impl FileTree for ControlTree {
    fn reset(&mut self) -> r9p::Result<()> {
        self.query_responses.clear();
        Ok(())
    }

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
        let access = mode & ACCESS_MASK;
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
            ControlNode::Usage => read_bytes(USAGE.as_bytes(), offset, count),
            ControlNode::Status => {
                let client = self.client.snapshot().map_err(p9_error)?;
                let response = status_json(
                    &client,
                    &self.config,
                    &self.feed_state,
                    &self.cache,
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
                let client = self.client.snapshot().map_err(p9_error)?;
                let response = match query::parse_json(data) {
                    Ok(request) => query::response_json(
                        &client,
                        &self.config,
                        &self.feed_state,
                        &self.cache,
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

fn walk_child(node: &ControlNode, name: &[u8]) -> Option<ControlNode> {
    let name = String::from_utf8_lossy(name).to_string();
    match node {
        ControlNode::Root => match name.as_str() {
            "usage" => Some(ControlNode::Usage),
            "status" => Some(ControlNode::Status),
            "query" => Some(ControlNode::Query),
            _ => None,
        },
        ControlNode::Usage | ControlNode::Status | ControlNode::Query => None,
    }
}

fn qid_for_path(path: u64, node: &ControlNode) -> Qid {
    if matches!(node, ControlNode::Root) {
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
    }
}

fn is_dir_node(node: &ControlNode) -> bool {
    matches!(node, ControlNode::Root)
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
    use super::{walk_child, ControlNode};

    #[test]
    fn root_exposes_only_the_current_control_surface() {
        assert_eq!(
            walk_child(&ControlNode::Root, b"status"),
            Some(ControlNode::Status)
        );
        assert_eq!(
            walk_child(&ControlNode::Root, b"query"),
            Some(ControlNode::Query)
        );
        assert_eq!(walk_child(&ControlNode::Root, b"snapshot"), None);
    }
}
