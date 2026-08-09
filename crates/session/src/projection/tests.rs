use super::{NamespaceProjection, NamespaceProjectionConfig};
use crate::{Client, ConnectionAuthentication, ConnectionConfig};
use r9p::{
    fid::Fid,
    qid::{Qid, DMDIR},
    server::{serve_file_tree_connection, FileTree, OpenFile, ReadData, ServerConfig},
    stat::Stat,
    ORDWR,
};
use std::{
    collections::BTreeMap,
    fs,
    net::TcpListener,
    path::PathBuf,
    process,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const ROOT: Qid = Qid::dir(1);
const MCP: Qid = Qid::dir(2);
const ECHO: Qid = Qid::file(3);

#[derive(Clone, Copy)]
enum Node {
    Root,
    Mcp,
    Echo,
}

struct EchoTree {
    fids: BTreeMap<Fid, Node>,
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl EchoTree {
    fn new(bytes: Arc<Mutex<Vec<u8>>>) -> Self {
        Self {
            fids: BTreeMap::new(),
            bytes,
        }
    }

    fn qid(node: Node) -> Qid {
        match node {
            Node::Root => ROOT,
            Node::Mcp => MCP,
            Node::Echo => ECHO,
        }
    }

    fn walk_one(node: Node, name: &[u8]) -> Option<Node> {
        match (node, name) {
            (node, b".") => Some(node),
            (Node::Root, b"..") => Some(Node::Root),
            (Node::Mcp, b"..") => Some(Node::Root),
            (Node::Echo, b"..") => Some(Node::Mcp),
            (Node::Root, b"mcp") => Some(Node::Mcp),
            (Node::Mcp, b"echo") => Some(Node::Echo),
            _ => None,
        }
    }
}

impl FileTree for EchoTree {
    fn attach(&mut self, fid: Fid, _uname: &[u8], _aname: &[u8]) -> r9p::Result<Qid> {
        self.fids.insert(fid, Node::Root);
        Ok(ROOT)
    }

    fn walk(
        &mut self,
        fid: Fid,
        newfid: Fid,
        _start: Qid,
        names: &[Vec<u8>],
    ) -> r9p::Result<Vec<Qid>> {
        let mut node = *self
            .fids
            .get(&fid)
            .ok_or_else(|| r9p::Error::from_static("unknown test fid"))?;
        let mut qids = Vec::with_capacity(names.len());
        for name in names {
            let Some(next) = Self::walk_one(node, name) else {
                break;
            };
            node = next;
            qids.push(Self::qid(node));
        }
        if qids.len() == names.len() {
            self.fids.insert(newfid, node);
        }
        Ok(qids)
    }

    fn open(&mut self, _fid: Fid, qid: Qid, _mode: u8) -> r9p::Result<OpenFile> {
        Ok(OpenFile { qid, iounit: 0 })
    }

    fn read(&mut self, _fid: Fid, qid: Qid, offset: u64, count: u32) -> r9p::Result<ReadData> {
        if qid != ECHO {
            return Ok(ReadData::Bytes(Vec::new()));
        }
        let bytes = self
            .bytes
            .lock()
            .map_err(|_| r9p::Error::from_static("test bytes poisoned"))?;
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        let end = start.saturating_add(count as usize).min(bytes.len());
        Ok(ReadData::Bytes(bytes[start..end].to_vec()))
    }

    fn write(&mut self, _fid: Fid, qid: Qid, offset: u64, data: &[u8]) -> r9p::Result<u32> {
        if qid != ECHO {
            return Err(r9p::Error::from_static("test path is not writable"));
        }
        let offset = usize::try_from(offset)
            .map_err(|_| r9p::Error::from_static("test write offset overflow"))?;
        let mut bytes = self
            .bytes
            .lock()
            .map_err(|_| r9p::Error::from_static("test bytes poisoned"))?;
        if bytes.len() < offset {
            bytes.resize(offset, 0);
        }
        let end = offset
            .checked_add(data.len())
            .ok_or_else(|| r9p::Error::from_static("test write length overflow"))?;
        if bytes.len() < end {
            bytes.resize(end, 0);
        }
        bytes[offset..end].copy_from_slice(data);
        u32::try_from(data.len()).map_err(|_| r9p::Error::from_static("test write too large"))
    }

    fn stat(&mut self, qid: Qid) -> r9p::Result<Stat> {
        match qid {
            ROOT => Ok(Stat::new(".", ROOT, DMDIR | 0o500)),
            MCP => Ok(Stat::new("mcp", MCP, DMDIR | 0o500)),
            ECHO => Ok(Stat::new("echo", ECHO, 0o600)),
            _ => Err(r9p::Error::from_static("unknown test qid")),
        }
    }

    fn clunk(&mut self, fid: Fid, _qid: Qid) -> r9p::Result<()> {
        self.fids.remove(&fid);
        Ok(())
    }
}

#[test]
fn projects_only_the_selected_subtree_over_a_private_socket() {
    let upstream = TcpListener::bind("127.0.0.1:0").expect("bind upstream");
    let upstream_address = upstream.local_addr().expect("upstream address");
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let server_bytes = Arc::clone(&bytes);
    let server = thread::spawn(move || {
        let (stream, _) = upstream.accept().expect("accept upstream");
        serve_file_tree_connection(stream, ServerConfig::default(), EchoTree::new(server_bytes))
            .expect("serve upstream");
    });

    let socket = unique_socket_path();
    let projection = NamespaceProjection::start(NamespaceProjectionConfig {
        socket: socket.clone(),
        namespace: connection(upstream_address.to_string()),
        source: "/mcp".to_string(),
        max_sessions: 2,
        max_async_requests: 8,
        connect_timeout: Duration::from_secs(2),
        operation_timeout: Duration::from_secs(2),
    })
    .expect("start projection");
    assert_eq!(
        fs::metadata(&socket)
            .expect("projection metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    let client = Client::connect_with_timeout(
        &connection(format!("unix!{}", socket.display())),
        Duration::from_secs(2),
    )
    .expect("connect projection");
    let writer = client
        .walk_path_timeout("/echo", Duration::from_secs(2))
        .expect("walk projected file");
    client
        .open_timeout(writer, ORDWR, Duration::from_secs(2))
        .expect("open projected writer");
    assert_eq!(
        client
            .write(writer, 0, b"namespace-native")
            .expect("write projected bytes"),
        16
    );

    let reader = client
        .walk_path_timeout("/echo", Duration::from_secs(2))
        .expect("walk projected reader");
    client
        .open_timeout(reader, ORDWR, Duration::from_secs(2))
        .expect("open projected reader");
    assert_eq!(
        client
            .read_timeout(reader, 0, 64, Duration::from_secs(2))
            .expect("read projected bytes"),
        b"namespace-native"
    );
    assert!(client
        .walk_path_timeout("/mcp", Duration::from_secs(2))
        .is_err());

    client.shutdown().expect("shutdown local client");
    drop(client);
    drop(projection);
    server.join().expect("upstream server should stop");
    assert!(!socket.exists());
}

fn connection(address: String) -> ConnectionConfig {
    ConnectionConfig {
        address,
        uname: "projection-test".to_string(),
        aname: String::new(),
        msize: 8192,
        authentication: ConnectionAuthentication::Unauthenticated,
    }
}

fn unique_socket_path() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!("r9p-projection-{}-{nonce}.sock", process::id()))
}

use std::os::unix::fs::PermissionsExt;
