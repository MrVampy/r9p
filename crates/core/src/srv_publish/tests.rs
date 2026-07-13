use super::*;
use crate::{
    blocking::OTRUNC,
    codec,
    export_descriptor::{AuthBoundary, ExportMode, Protocol, TransportClass},
    message::TMessage,
    qid::{Qid, DMDIR},
    server::{FileTree, OpenFile, ReadData, Server},
    stat::Stat,
};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
    thread,
};

#[test]
fn publishes_missing_srv_entry() {
    let tree = SharedSrvTree::new();
    let address = serve_tree(tree.clone());
    let mut publication = publication(&address);
    publication.vault_endpoint_bind = address;

    let outcome = publish_r9p_export(&publication).expect("publish should succeed");

    assert_eq!(outcome, PublishOutcome::Registered);
    let descriptor = tree
        .content("polymarket")
        .expect("descriptor should be written");
    assert!(descriptor.contains("format\tr9p-export.v1\n"));
    assert!(descriptor.contains("endpoint_bind\t192.168.0.21:19590\n"));
}

#[test]
fn publication_descriptor_can_carry_host_ownership() {
    let tree = SharedSrvTree::new();
    let address = serve_tree(tree.clone());
    let mut publication = publication(&address);
    publication.vault_endpoint_bind = address;
    publication.descriptor.extra_fields.insert(
        "service_unit".to_string(),
        "vault-polymarket-watcher.service".to_string(),
    );
    publication.descriptor.extra_fields.insert(
        "host_firewall_admission".to_string(),
        "tcp:192.168.0.21:19590".to_string(),
    );

    let outcome = publish_r9p_export(&publication).expect("publish should succeed");

    assert_eq!(outcome, PublishOutcome::Registered);
    let descriptor = tree
        .content("polymarket")
        .expect("descriptor should be written");
    assert!(descriptor.contains("service_unit\tvault-polymarket-watcher.service\n"));
    assert!(descriptor.contains("host_firewall_admission\ttcp:192.168.0.21:19590\n"));
}

#[test]
fn publish_is_idempotent_when_ready_summary_matches() {
    let tree = SharedSrvTree::new();
    tree.set_ready_summary("polymarket", ready_summary("192.168.0.21:19590"));
    let address = serve_tree(tree.clone());
    let mut publication = publication(&address);
    publication.vault_endpoint_bind = address;

    let outcome = publish_r9p_export(&publication).expect("publish should succeed");

    assert_eq!(outcome, PublishOutcome::AlreadyReady);
    let content = tree
        .content("polymarket")
        .expect("descriptor renewal should be written");
    assert_eq!(
        content,
        publication
            .descriptor
            .render()
            .expect("descriptor should render")
    );
}

#[test]
fn publish_updates_stale_ready_summary_in_place() {
    let tree = SharedSrvTree::new();
    tree.set_ready_summary("polymarket", ready_summary("192.168.0.21:19591"));
    let before_id = tree.file_id("polymarket").expect("ready file id");
    let address = serve_tree(tree.clone());
    let mut publication = publication(&address);
    publication.vault_endpoint_bind = address;

    let outcome = publish_r9p_export(&publication).expect("publish should succeed");

    assert_eq!(outcome, PublishOutcome::Updated);
    assert_eq!(tree.file_id("polymarket"), Some(before_id));
    let descriptor = tree
        .content("polymarket")
        .expect("descriptor should be written");
    assert!(descriptor.contains("endpoint_bind\t192.168.0.21:19590\n"));
}

#[test]
fn matching_summary_uses_vault_transport_class() {
    let publication = publication("127.0.0.1:9564");
    assert!(
        ready_summary_matches(&ready_summary("192.168.0.21:19590"), &publication)
            .expect("summary should compare")
    );
}

#[test]
fn maintainer_wait_paths_use_srv_namespace() {
    assert_eq!(
        srv_wait_state_path("polymarket"),
        "/srv/wait/polymarket/state"
    );
    assert_eq!(
        srv_wait_state_path("kalshi/demo/actuator"),
        "/srv/wait/kalshi/demo/actuator/state"
    );
    assert_eq!(
        srv_wait_changed_after_path("polymarket", "token-1"),
        "/srv/wait/polymarket/changed-after/token-1"
    );
    assert_eq!(
        srv_wait_changed_after_path("kalshi/demo/actuator", "token-1"),
        "/srv/wait/kalshi/demo/actuator/changed-after/token-1"
    );
}

#[test]
fn nested_srv_publication_creates_from_srv_root_with_full_service_name() {
    assert_eq!(
        srv_create_parent_and_name("kalshi/demo/actuator").expect("valid nested name"),
        ("/srv", "kalshi/demo/actuator")
    );
}

#[test]
fn srv_directory_at_registration_path_is_missing_registration() {
    let tree = SharedSrvTree::new();
    tree.ensure_srv_dir_path(&["kalshi", "demo", "actuator"]);
    let address = serve_tree(tree.clone());
    let mut client =
        Client::connect_tcp(&address, "codex", "/", 65_536).expect("connect test client");

    let state =
        inspect_srv_path(&mut client, "/srv/kalshi/demo/actuator").expect("inspect should succeed");

    assert_eq!(state, SrvPathState::Missing);
}

#[test]
fn publishes_missing_nested_srv_entry() {
    let tree = SharedSrvTree::new();
    let address = serve_tree(tree.clone());
    let mut publication = publication(&address);
    publication.vault_endpoint_bind = address;
    publication.service_name = "kalshi/demo/actuator".to_string();

    let outcome = publish_r9p_export(&publication).expect("publish should succeed");

    assert_eq!(outcome, PublishOutcome::Registered);
    let descriptor = tree
        .content_path(&["kalshi", "demo", "actuator"])
        .expect("descriptor should be written");
    assert!(descriptor.contains("format\tr9p-export.v1\n"));
    assert!(descriptor.contains("endpoint_bind\t192.168.0.21:19590\n"));
}

#[test]
fn maintainer_republishes_disappeared_srv_entry() {
    let tree = SharedSrvTree::new();
    let address = serve_tree(tree.clone());
    let mut publication = publication(&address);
    publication.vault_endpoint_bind = address;

    let maintainer = maintain_r9p_export(
        publication,
        R9pExportMaintenanceConfig {
            retry_interval: Duration::from_secs(60),
        },
    )
    .expect("maintainer should start");
    tree.remove_file("polymarket");
    maintainer.reconcile_now();

    wait_for_descriptor(&tree, "polymarket");
    let status = maintainer.status();
    assert!(status.success_count >= 2);
    assert_eq!(status.last_error, None);
    maintainer.shutdown();
}

#[test]
fn maintainer_waits_before_reconciling_after_initial_publication() {
    let tree = SharedSrvTree::new();
    tree.set_ready_summary("polymarket", ready_summary("192.168.0.21:19590"));
    let address = serve_tree(tree.clone());
    let mut publication = publication(&address);
    publication.vault_endpoint_bind = address;
    let expected_descriptor = publication
        .descriptor
        .render()
        .expect("descriptor should render");

    let maintainer = maintain_r9p_export(
        publication,
        R9pExportMaintenanceConfig {
            retry_interval: Duration::from_secs(60),
        },
    )
    .expect("maintainer should start");
    thread::sleep(Duration::from_millis(100));

    let content = tree
        .content("polymarket")
        .expect("initial descriptor renewal should remain current");
    assert_eq!(content, expected_descriptor);
    assert_eq!(maintainer.status().success_count, 1);
    maintainer.shutdown();
}

#[test]
fn wait_transport_timeout_requests_renewal_without_failure() {
    assert!(looks_timeout(&Error::from(
        "read response: 9P transport timeout or would-block: timed out",
    )));
    assert!(looks_timeout(&Error::from(
        "read response: 9P transport timeout or would-block: operation would block",
    )));
    assert!(!looks_timeout(&Error::from("file does not exist")));
}

fn publication(vault_endpoint_bind: &str) -> R9pExportPublication {
    R9pExportPublication {
        vault_endpoint_bind: vault_endpoint_bind.to_string(),
        vault_uname: "codex".to_string(),
        vault_aname: "/".to_string(),
        service_name: "polymarket".to_string(),
        descriptor: ExportDescriptor {
            endpoint_bind: "192.168.0.21:19590".to_string(),
            aname: "/".to_string(),
            uname: "codex".to_string(),
            exported_root: "/".to_string(),
            transport_class: TransportClass::Tcp,
            mode: ExportMode::ReadOnly,
            auth: AuthBoundary::parse("wg:vault-runtime-lan").expect("auth should parse"),
            pid: 1234,
            protocol: Protocol::NineP2000,
            msize: 65_536,
            expires_at: None,
            local_root_label: Some("polymarket-watcher".to_string()),
            namespace_mount_paths: Vec::new(),
            extra_fields: BTreeMap::new(),
        },
    }
}

fn ready_summary(endpoint: &str) -> String {
    [
        "service: polymarket",
        "owner: codex.interface",
        "channel_kind: peer_namespace",
        "channel: r9p-export:polymarket",
        &format!(
            "endpoint: inline:r9p-export:polymarket:{endpoint}:codex:network_class:vault-runtime-lan"
        ),
        "aname: /",
        "exported_root: /",
        "lease_id: srv-lease:polymarket:r9p-export:polymarket",
        "lease_ttl_ms: 300000",
        "created_at_ms: 1",
        "attached_at_ms: 2",
        "",
    ]
    .join("\n")
}

#[derive(Clone)]
struct SharedSrvTree {
    inner: Arc<Mutex<SrvTree>>,
}

impl SharedSrvTree {
    fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(SrvTree::new())),
        }
    }

    fn set_ready_summary(&self, name: &str, content: String) {
        self.inner
            .lock()
            .expect("tree lock")
            .set_file(name.as_bytes(), content.into_bytes());
    }

    fn content(&self, name: &str) -> Option<String> {
        self.inner
            .lock()
            .expect("tree lock")
            .file_content(name.as_bytes())
            .map(|bytes| String::from_utf8(bytes).expect("utf-8 content"))
    }

    fn file_id(&self, name: &str) -> Option<u64> {
        self.inner
            .lock()
            .expect("tree lock")
            .file_id(name.as_bytes())
    }

    fn remove_file(&self, name: &str) {
        self.inner
            .lock()
            .expect("tree lock")
            .remove_file(name.as_bytes());
    }

    fn ensure_srv_dir_path(&self, segments: &[&str]) {
        self.inner
            .lock()
            .expect("tree lock")
            .ensure_srv_dir_path(segments);
    }

    fn content_path(&self, segments: &[&str]) -> Option<String> {
        self.inner
            .lock()
            .expect("tree lock")
            .file_content_path(segments)
            .map(|bytes| String::from_utf8(bytes).expect("utf-8 content"))
    }
}

impl FileTree for SharedSrvTree {
    fn attach(&mut self, _fid: u32, _uname: &[u8], _aname: &[u8]) -> Result<Qid> {
        Ok(Qid::dir(ROOT))
    }

    fn walk(&mut self, _fid: u32, _newfid: u32, start: Qid, names: &[Vec<u8>]) -> Result<Vec<Qid>> {
        self.inner
            .lock()
            .expect("tree lock")
            .walk(start.path, names)
    }

    fn open(&mut self, _fid: u32, qid: Qid, mode: u8) -> Result<OpenFile> {
        if mode & OTRUNC != 0 {
            self.inner.lock().expect("tree lock").truncate(qid.path)?;
        }
        Ok(OpenFile { qid, iounit: 0 })
    }

    fn read(&mut self, _fid: u32, qid: Qid, offset: u64, count: u32) -> Result<ReadData> {
        self.inner
            .lock()
            .expect("tree lock")
            .read(qid.path, offset, count)
    }

    fn stat(&mut self, qid: Qid) -> Result<Stat> {
        self.inner.lock().expect("tree lock").stat(qid.path)
    }

    fn create(
        &mut self,
        _fid: u32,
        qid: Qid,
        name: &[u8],
        _perm: u32,
        _mode: u8,
    ) -> Result<OpenFile> {
        self.inner.lock().expect("tree lock").create(qid.path, name)
    }

    fn write(&mut self, _fid: u32, qid: Qid, offset: u64, data: &[u8]) -> Result<u32> {
        self.inner
            .lock()
            .expect("tree lock")
            .write(qid.path, offset, data)
    }

    fn remove(&mut self, _fid: u32, qid: Qid) -> Result<()> {
        self.inner.lock().expect("tree lock").remove(qid.path)
    }
}

const ROOT: u64 = 1;
const SRV: u64 = 2;

struct SrvTree {
    nodes: BTreeMap<u64, TestNode>,
    next_id: u64,
}

struct TestNode {
    name: Vec<u8>,
    parent: u64,
    body: TestBody,
}

enum TestBody {
    Dir(BTreeMap<Vec<u8>, u64>),
    File(Vec<u8>),
}

impl SrvTree {
    fn new() -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert(
            ROOT,
            TestNode {
                name: b".".to_vec(),
                parent: ROOT,
                body: TestBody::Dir(BTreeMap::from([(b"srv".to_vec(), SRV)])),
            },
        );
        nodes.insert(
            SRV,
            TestNode {
                name: b"srv".to_vec(),
                parent: ROOT,
                body: TestBody::Dir(BTreeMap::new()),
            },
        );
        Self { nodes, next_id: 3 }
    }

    fn walk(&self, start: u64, names: &[Vec<u8>]) -> Result<Vec<Qid>> {
        let mut current = start;
        let mut qids = Vec::new();
        for name in names {
            if name == b"." {
                qids.push(self.qid(current)?);
                continue;
            }
            if name == b".." {
                current = self.node(current)?.parent;
                qids.push(self.qid(current)?);
                continue;
            }
            let node = self.node(current)?;
            let TestBody::Dir(children) = &node.body else {
                break;
            };
            let Some(next) = children.get(name).copied() else {
                break;
            };
            current = next;
            qids.push(self.qid(current)?);
        }
        Ok(qids)
    }

    fn create(&mut self, parent: u64, name: &[u8]) -> Result<OpenFile> {
        if parent == SRV && name.contains(&b'/') {
            return self.create_srv_service_file(name);
        }
        self.create_file(parent, name)
    }

    fn create_srv_service_file(&mut self, name: &[u8]) -> Result<OpenFile> {
        let service_name = std::str::from_utf8(name)
            .map_err(|error| Error::from(format!("invalid service name utf-8: {error}")))?;
        let mut segments = service_name.split('/').collect::<Vec<_>>();
        let leaf = segments
            .pop()
            .ok_or_else(|| Error::from("invalid empty service name"))?;
        let mut parent = SRV;
        for segment in segments {
            parent = match self.child(parent, segment.as_bytes()) {
                Some(id) => id,
                None => self.insert_dir(parent, segment.as_bytes()),
            };
        }
        self.create_file(parent, leaf.as_bytes())
    }

    fn create_file(&mut self, parent: u64, name: &[u8]) -> Result<OpenFile> {
        let parent_node = self
            .nodes
            .get_mut(&parent)
            .ok_or_else(|| Error::from("missing parent"))?;
        let TestBody::Dir(children) = &mut parent_node.body else {
            return Err(Error::from("not a directory"));
        };
        if children.contains_key(name) {
            return Err(Error::from("file exists"));
        }
        let id = self.next_id;
        self.next_id += 1;
        children.insert(name.to_vec(), id);
        self.nodes.insert(
            id,
            TestNode {
                name: name.to_vec(),
                parent,
                body: TestBody::File(Vec::new()),
            },
        );
        Ok(OpenFile {
            qid: Qid::file(id),
            iounit: 0,
        })
    }

    fn set_file(&mut self, name: &[u8], content: Vec<u8>) {
        if let Some(id) = self.child(SRV, name) {
            if let Some(TestNode {
                body: TestBody::File(bytes),
                ..
            }) = self.nodes.get_mut(&id)
            {
                *bytes = content;
                return;
            }
        }
        let id = self.next_id;
        self.next_id += 1;
        if let Some(TestNode {
            body: TestBody::Dir(children),
            ..
        }) = self.nodes.get_mut(&SRV)
        {
            children.insert(name.to_vec(), id);
        }
        self.nodes.insert(
            id,
            TestNode {
                name: name.to_vec(),
                parent: SRV,
                body: TestBody::File(content),
            },
        );
    }

    fn ensure_srv_dir_path(&mut self, segments: &[&str]) {
        let mut current = SRV;
        for segment in segments {
            current = match self.child(current, segment.as_bytes()) {
                Some(id) => id,
                None => self.insert_dir(current, segment.as_bytes()),
            };
        }
    }

    fn insert_dir(&mut self, parent: u64, name: &[u8]) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        if let Some(TestNode {
            body: TestBody::Dir(children),
            ..
        }) = self.nodes.get_mut(&parent)
        {
            children.insert(name.to_vec(), id);
        }
        self.nodes.insert(
            id,
            TestNode {
                name: name.to_vec(),
                parent,
                body: TestBody::Dir(BTreeMap::new()),
            },
        );
        id
    }

    fn file_content(&self, name: &[u8]) -> Option<Vec<u8>> {
        let id = self.child(SRV, name)?;
        match &self.nodes.get(&id)?.body {
            TestBody::File(bytes) => Some(bytes.clone()),
            TestBody::Dir(_) => None,
        }
    }

    fn file_content_path(&self, segments: &[&str]) -> Option<Vec<u8>> {
        let id = self.child_path(SRV, segments)?;
        match &self.nodes.get(&id)?.body {
            TestBody::File(bytes) => Some(bytes.clone()),
            TestBody::Dir(_) => None,
        }
    }

    fn file_id(&self, name: &[u8]) -> Option<u64> {
        self.child(SRV, name)
    }

    fn read(&self, id: u64, offset: u64, count: u32) -> Result<ReadData> {
        match &self.node(id)?.body {
            TestBody::File(bytes) => {
                let offset = usize::try_from(offset).unwrap_or(usize::MAX);
                let count = usize::try_from(count).unwrap_or(usize::MAX);
                let end = offset.saturating_add(count).min(bytes.len());
                Ok(ReadData::Bytes(if offset >= bytes.len() {
                    Vec::new()
                } else {
                    bytes[offset..end].to_vec()
                }))
            }
            TestBody::Dir(children) => Ok(ReadData::Directory(
                children
                    .values()
                    .filter_map(|id| self.stat(*id).ok())
                    .collect(),
            )),
        }
    }

    fn write(&mut self, id: u64, offset: u64, data: &[u8]) -> Result<u32> {
        let node = self
            .nodes
            .get_mut(&id)
            .ok_or_else(|| Error::from("missing file"))?;
        let TestBody::File(bytes) = &mut node.body else {
            return Err(Error::from("not a file"));
        };
        let offset = usize::try_from(offset).map_err(|_| Error::from("offset overflow"))?;
        if bytes.len() < offset {
            bytes.resize(offset, 0);
        }
        if bytes.len() < offset + data.len() {
            bytes.resize(offset + data.len(), 0);
        }
        bytes[offset..offset + data.len()].copy_from_slice(data);
        u32::try_from(data.len()).map_err(|_| Error::from("write too large"))
    }

    fn truncate(&mut self, id: u64) -> Result<()> {
        let node = self
            .nodes
            .get_mut(&id)
            .ok_or_else(|| Error::from("missing file"))?;
        let TestBody::File(bytes) = &mut node.body else {
            return Err(Error::from("not a file"));
        };
        bytes.clear();
        Ok(())
    }

    fn remove(&mut self, id: u64) -> Result<()> {
        if id == ROOT || id == SRV {
            return Err(Error::from("cannot remove directory"));
        }
        let parent = self.node(id)?.parent;
        let name = self.node(id)?.name.clone();
        if let Some(TestNode {
            body: TestBody::Dir(children),
            ..
        }) = self.nodes.get_mut(&parent)
        {
            children.remove(&name);
        }
        self.nodes.remove(&id);
        Ok(())
    }

    fn remove_file(&mut self, name: &[u8]) {
        if let Some(id) = self.child(SRV, name) {
            let _ = self.remove(id);
        }
    }

    fn stat(&self, id: u64) -> Result<Stat> {
        let node = self.node(id)?;
        match &node.body {
            TestBody::Dir(_) => Ok(Stat::new(node.name.clone(), Qid::dir(id), DMDIR | 0o555)),
            TestBody::File(bytes) => {
                let mut stat = Stat::new(node.name.clone(), Qid::file(id), 0o666);
                stat.length = bytes.len() as u64;
                Ok(stat)
            }
        }
    }

    fn qid(&self, id: u64) -> Result<Qid> {
        match self.node(id)?.body {
            TestBody::Dir(_) => Ok(Qid::dir(id)),
            TestBody::File(_) => Ok(Qid::file(id)),
        }
    }

    fn node(&self, id: u64) -> Result<&TestNode> {
        self.nodes
            .get(&id)
            .ok_or_else(|| Error::from("file does not exist"))
    }

    fn child(&self, parent: u64, name: &[u8]) -> Option<u64> {
        let TestBody::Dir(children) = &self.nodes.get(&parent)?.body else {
            return None;
        };
        children.get(name).copied()
    }

    fn child_path(&self, start: u64, segments: &[&str]) -> Option<u64> {
        let mut current = start;
        for segment in segments {
            current = self.child(current, segment.as_bytes())?;
        }
        Some(current)
    }
}

fn serve_tree(tree: SharedSrvTree) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
    let address = listener.local_addr().expect("local addr").to_string();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let connection_tree = tree.clone();
            thread::spawn(move || {
                let _ = serve_connection(connection_tree, &mut stream);
            });
        }
    });
    address
}

fn wait_for_descriptor(tree: &SharedSrvTree, name: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if tree
            .content(name)
            .map(|content| content.contains("format\tr9p-export.v1\n"))
            .unwrap_or(false)
        {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("descriptor was not republished");
}

fn serve_connection(tree: SharedSrvTree, stream: &mut TcpStream) -> Result<()> {
    let mut server = Server::new(tree);
    loop {
        let mut prefix = [0_u8; 4];
        if stream.read_exact(&mut prefix).is_err() {
            return Ok(());
        }
        let size = u32::from_le_bytes(prefix);
        let rest_len = usize::try_from(size - 4).map_err(|_| Error::from("frame too large"))?;
        let mut frame = Vec::with_capacity(size as usize);
        frame.extend(prefix);
        frame.resize(size as usize, 0);
        stream
            .read_exact(&mut frame[4..4 + rest_len])
            .map_err(|error| Error::from(format!("read request: {error}")))?;
        let request = codec::decode_tmessage(&frame)?;
        let reply = match request {
            TMessage::Version { .. } => server.handle(request),
            _ => server.handle(request),
        };
        let encoded = codec::encode_rmessage(&reply)?;
        stream
            .write_all(&encoded)
            .map_err(|error| Error::from(format!("write reply: {error}")))?;
    }
}
