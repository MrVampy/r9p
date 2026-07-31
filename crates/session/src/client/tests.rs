use super::Client;
use crate::ConnectionConfig;
use r9p::{
    codec::{self, Variant},
    error::{Error as P9Error, Result as P9Result},
    fid::Fid,
    qid::{Qid, DMDIR},
    referral::NamespaceReferral,
    server::{FileTree, OpenFile, ReadData, Server, ServerConfig},
    stat::{decode_dir_entries, Stat},
};
use std::{
    env, fs,
    io::{self, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    os::unix::net::UnixListener,
    path::{Path, PathBuf},
    process,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

static ENV_LOCK: Mutex<()> = Mutex::new(());
const ROOT_QID: Qid = Qid::dir(1);

fn connection(address: String) -> ConnectionConfig {
    ConnectionConfig {
        address,
        uname: "codex".to_string(),
        aname: "/".to_string(),
        msize: 8192,
        auth_config: None,
        authorities: crate::AuthorityBindings::new(),
    }
}

#[test]
fn connects_explicit_unix_socket() {
    let socket_path = unique_socket_path("explicit");
    let server = spawn_unix_root_server(&socket_path);
    let client = Client::connect_with_timeout(
        &connection(format!("unix!{}", socket_path.display())),
        Duration::ZERO,
    )
    .expect("client should connect");

    let stat = client
        .stat_timeout(client.root_fid(), Duration::from_secs(1))
        .expect("root stat should succeed");
    assert_eq!(stat.name, b".".to_vec());

    let root = client
        .clone_fid_timeout(client.root_fid(), Duration::from_secs(1))
        .expect("root fid should clone");
    client
        .open_timeout(root, r9p::blocking::OREAD, Duration::from_secs(1))
        .expect("root directory should open");
    let bytes = client
        .read_full(root, 0, 8192)
        .expect("blocking full read should succeed");
    assert!(bytes.is_empty());
    client
        .clunk_timeout(root, Duration::from_secs(1))
        .expect("root clone should clunk");
    client.shutdown().expect("session should shut down");

    drop(client);
    server.join().expect("server should not panic");
    let _ = fs::remove_file(socket_path);
}

#[test]
fn connect_waits_for_unix_socket_to_appear() {
    let socket_path = unique_socket_path("delayed");
    let server_path = socket_path.clone();
    let server = thread::spawn(move || {
        thread::sleep(Duration::from_millis(100));
        let listener = UnixListener::bind(server_path).expect("unix listener should bind");
        let (stream, _) = listener.accept().expect("server should accept");
        handle_connection(stream).expect("server connection should complete");
    });

    let client = Client::connect_with_timeout(
        &connection(format!("unix!{}", socket_path.display())),
        Duration::from_secs(2),
    )
    .expect("client should wait for socket and connect");
    let stat = client
        .stat_timeout(client.root_fid(), Duration::from_secs(1))
        .expect("root stat should succeed");
    assert_eq!(stat.name, b".".to_vec());

    drop(client);
    server.join().expect("server should not panic");
    let _ = fs::remove_file(socket_path);
}

#[test]
fn connect_waits_for_tcp_listener_to_appear() {
    let reservation = TcpListener::bind("127.0.0.1:0").expect("reserve TCP address");
    let address = reservation.local_addr().expect("inspect TCP address");
    drop(reservation);
    let server = thread::spawn(move || {
        thread::sleep(Duration::from_millis(100));
        let listener = TcpListener::bind(address).expect("TCP listener should bind");
        let (stream, _) = listener.accept().expect("server should accept");
        handle_connection(stream).expect("server connection should complete");
    });

    let client =
        Client::connect_with_timeout(&connection(address.to_string()), Duration::from_secs(2))
            .expect("client should wait for TCP listener and connect");
    let stat = client
        .stat_timeout(client.root_fid(), Duration::from_secs(1))
        .expect("root stat should succeed");
    assert_eq!(stat.name, b".".to_vec());

    drop(client);
    server.join().expect("server should not panic");
}

#[test]
fn connects_namespace_socket() {
    let _env = ENV_LOCK.lock().expect("env lock should not be poisoned");
    let namespace = unique_namespace_dir("namespace");
    fs::create_dir_all(&namespace).expect("namespace dir should be created");
    let socket_path = namespace.join("example-service");
    let previous = env::var("NAMESPACE").ok();
    env::set_var("NAMESPACE", &namespace);
    let server = spawn_unix_root_server(&socket_path);

    let client = Client::connect_with_timeout(
        &connection("namespace!example-service".to_string()),
        Duration::ZERO,
    )
    .expect("client should connect");
    let stat = client
        .stat_timeout(client.root_fid(), Duration::from_secs(1))
        .expect("root stat should succeed");
    assert_eq!(stat.name, b".".to_vec());

    drop(client);
    server.join().expect("server should not panic");
    if let Some(previous) = previous {
        env::set_var("NAMESPACE", previous);
    } else {
        env::remove_var("NAMESPACE");
    }
    let _ = fs::remove_file(socket_path);
    let _ = fs::remove_dir(namespace);
}

#[test]
fn client_session_reconnects_once_and_bumps_its_epoch() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("session listener");
    let address = listener.local_addr().expect("session address");
    let server = thread::spawn(move || {
        let mut handlers = Vec::new();
        for _ in 0..2 {
            let (stream, _) = listener.accept().expect("session accept");
            handlers.push(thread::spawn(move || {
                handle_connection(stream).expect("session connection")
            }));
        }
        for handler in handlers {
            handler.join().expect("session handler should not panic");
        }
    });

    let session =
        crate::ClientSession::connect(&connection(address.to_string()), Duration::from_secs(1))
            .expect("client session should connect");
    let first = session.snapshot().expect("first attachment");
    let first_epoch = session.session_epoch().expect("first epoch");

    let replacement = session
        .reconnect_after(&first)
        .expect("failed attachment should be replaced");
    let second_epoch = session.session_epoch().expect("second epoch");
    assert_ne!(first_epoch, second_epoch);
    assert!(!replacement.same_session(&first));

    let shared = session
        .reconnect_after(&first)
        .expect("stale reconnect request should reuse the replacement");
    assert!(shared.same_session(&replacement));
    assert_eq!(session.session_epoch().expect("shared epoch"), second_epoch);

    session.shutdown().expect("replacement should shut down");
    first.shutdown().expect("first attachment should shut down");
    drop(shared);
    drop(replacement);
    drop(first);
    drop(session);
    server.join().expect("server should not panic");
}

#[test]
fn ordinary_namespace_operations_cross_referrals_transparently() {
    let service_listener = TcpListener::bind("127.0.0.1:0").expect("service listener");
    let service_address = service_listener.local_addr().expect("service address");
    let service = thread::spawn(move || {
        let (stream, _) = service_listener.accept().expect("service accept");
        handle_configured_connection(stream, ValueTree, ServerConfig::default())
            .expect("service connection");
    });

    let root_listener = TcpListener::bind("127.0.0.1:0").expect("root listener");
    let root_address = root_listener.local_addr().expect("root address");
    let root = thread::spawn(move || {
        let (stream, _) = root_listener.accept().expect("root accept");
        handle_configured_connection(
            stream,
            ReferralRoot {
                endpoint: service_address.to_string().into_bytes(),
                referral_reads: None,
            },
            ServerConfig {
                variant: Variant::R,
                ..ServerConfig::default()
            },
        )
        .expect("root connection");
    });

    let client = Client::connect_with_timeout(
        &connection(root_address.to_string()),
        Duration::from_secs(1),
    )
    .expect("namespace client should connect");
    assert_eq!(client.variant(), Variant::R);

    let sources = client
        .walk_one_timeout(client.root_fid(), b"sources", Duration::from_secs(1))
        .expect("referral ancestor should resolve as a directory");
    let sources_stat = client
        .stat_timeout(sources, Duration::from_secs(1))
        .expect("referral ancestor should have directory metadata");
    assert_eq!(sources_stat.name, b"sources");
    assert!(sources_stat.qid.is_dir());
    client
        .open_timeout(sources, r9p::OREAD, Duration::from_secs(1))
        .expect("referral ancestor directory should open");
    let entries = decode_dir_entries(
        &client
            .read_full_timeout(sources, 0, 8192, Duration::from_secs(1))
            .expect("referral ancestor directory should read"),
    )
    .expect("referral ancestor entries should decode");
    assert_eq!(
        entries
            .into_iter()
            .map(|entry| entry.name)
            .collect::<Vec<_>>(),
        vec![b"x".to_vec()]
    );
    let x = client
        .walk_one_timeout(sources, b"x", Duration::from_secs(1))
        .expect("referral mount should resolve from its synthetic parent");
    let value = client
        .walk_one_timeout(x, b"value", Duration::from_secs(1))
        .expect("referred service child should resolve component by component");
    assert_eq!(
        client
            .stat_timeout(value, Duration::from_secs(1))
            .expect("referred service child should stat")
            .name,
        b"value"
    );
    client.clunk(value).expect("value fid should clunk");
    client.clunk(x).expect("service root fid should clunk");
    client
        .clunk(sources)
        .expect("referral ancestor fid should clunk locally");

    assert_eq!(
        client
            .read_path_timeout("/sources/x/value", 8192, Duration::from_secs(1))
            .expect("bounded path read should route directly to the referred service"),
        b"direct-service-value"
    );
    let stat = client
        .stat_path_timeout("/sources/x/value", Duration::from_secs(1))
        .expect("bounded path stat should reuse the direct service");
    assert_eq!(stat.name, b"value");
    client.shutdown().expect("namespace should shut down");

    drop(client);
    root.join().expect("root server should not panic");
    service.join().expect("service server should not panic");
}

#[test]
fn missing_path_inside_selected_referral_does_not_refresh_routes() {
    let service_listener = TcpListener::bind("127.0.0.1:0").expect("service listener");
    let service_address = service_listener.local_addr().expect("service address");
    let service = thread::spawn(move || {
        let (stream, _) = service_listener.accept().expect("service accept");
        handle_configured_connection(stream, ValueTree, ServerConfig::default())
            .expect("service connection");
    });

    let referral_reads = Arc::new(AtomicUsize::new(0));
    let root_referral_reads = Arc::clone(&referral_reads);
    let root_listener = TcpListener::bind("127.0.0.1:0").expect("root listener");
    let root_address = root_listener.local_addr().expect("root address");
    let root = thread::spawn(move || {
        let (stream, _) = root_listener.accept().expect("root accept");
        handle_configured_connection(
            stream,
            ReferralRoot {
                endpoint: service_address.to_string().into_bytes(),
                referral_reads: Some(root_referral_reads),
            },
            ServerConfig {
                variant: Variant::R,
                ..ServerConfig::default()
            },
        )
        .expect("root connection");
    });

    let client = Client::connect_with_timeout(
        &connection(root_address.to_string()),
        Duration::from_secs(1),
    )
    .expect("namespace client should connect");
    assert_eq!(referral_reads.load(Ordering::SeqCst), 1);

    let error = client
        .walk_path_timeout("/sources/x/missing", Duration::from_secs(1))
        .expect_err("missing direct-service child must stay missing");
    assert_eq!(error.errno, libc::ENOENT);
    assert_eq!(
        referral_reads.load(Ordering::SeqCst),
        1,
        "a direct-service miss must not refresh the root referral table"
    );

    client.shutdown().expect("namespace should shut down");
    drop(client);
    root.join().expect("root server should not panic");
    service.join().expect("service server should not panic");
}

#[test]
fn read_only_path_recovers_a_restarted_referral_session() {
    let service_listener = TcpListener::bind("127.0.0.1:0").expect("service listener");
    let service_address = service_listener.local_addr().expect("service address");
    let service = thread::spawn(move || {
        let (first_stream, _) = service_listener.accept().expect("first service accept");
        let shutdown_stream = first_stream
            .try_clone()
            .expect("clone first service stream");
        let _ = handle_configured_connection(
            first_stream,
            ClosingStatTree {
                shutdown_stream: Some(shutdown_stream),
            },
            ServerConfig::default(),
        );

        let (second_stream, _) = service_listener.accept().expect("second service accept");
        handle_configured_connection(second_stream, ValueTree, ServerConfig::default())
            .expect("replacement service connection");
    });

    let root_listener = TcpListener::bind("127.0.0.1:0").expect("root listener");
    let root_address = root_listener.local_addr().expect("root address");
    let root = thread::spawn(move || {
        let (stream, _) = root_listener.accept().expect("root accept");
        handle_configured_connection(
            stream,
            ReferralRoot {
                endpoint: service_address.to_string().into_bytes(),
                referral_reads: None,
            },
            ServerConfig {
                variant: Variant::R,
                ..ServerConfig::default()
            },
        )
        .expect("root connection");
    });

    let client = Client::connect_with_timeout(
        &connection(root_address.to_string()),
        Duration::from_secs(1),
    )
    .expect("namespace client should connect");
    let stat = client
        .stat_path_timeout("/sources/x/value", Duration::from_secs(1))
        .expect("read-only path operation should reconnect through the same referral");
    assert_eq!(stat.name, b"value");
    client.shutdown().expect("namespace should shut down");

    drop(client);
    root.join().expect("root server should not panic");
    service.join().expect("service server should not panic");
}

#[test]
fn walk_miss_refreshes_referrals_added_after_attach() {
    let service_listener = TcpListener::bind("127.0.0.1:0").expect("service listener");
    let service_address = service_listener.local_addr().expect("service address");
    let service = thread::spawn(move || {
        let (stream, _) = service_listener.accept().expect("service accept");
        handle_configured_connection(stream, ValueTree, ServerConfig::default())
            .expect("service connection");
    });

    let root_listener = TcpListener::bind("127.0.0.1:0").expect("root listener");
    let root_address = root_listener.local_addr().expect("root address");
    let root = thread::spawn(move || {
        let (stream, _) = root_listener.accept().expect("root accept");
        handle_configured_connection(
            stream,
            AppearingReferralRoot {
                endpoint: service_address.to_string().into_bytes(),
                referral_reads: 0,
            },
            ServerConfig {
                variant: Variant::R,
                ..ServerConfig::default()
            },
        )
        .expect("root connection");
    });

    let client = Client::connect_with_timeout(
        &connection(root_address.to_string()),
        Duration::from_secs(1),
    )
    .expect("namespace client should attach before the referral exists");
    let sources = client
        .walk_one_timeout(client.root_fid(), b"sources", Duration::from_secs(1))
        .expect("local sources directory should resolve");
    let x = client
        .walk_one_timeout(sources, b"x", Duration::from_secs(1))
        .expect("walk miss should refresh and route the newly admitted service");
    let value = client
        .walk_one_timeout(x, b"value", Duration::from_secs(1))
        .expect("referred service child should resolve");
    client
        .open_timeout(value, r9p::blocking::OREAD, Duration::from_secs(1))
        .expect("referred value should open");
    assert_eq!(
        client
            .read_full_timeout(value, 0, 8192, Duration::from_secs(1))
            .expect("referred value should read"),
        b"direct-service-value"
    );
    client.shutdown().expect("namespace should shut down");

    drop(client);
    root.join().expect("root server should not panic");
    service.join().expect("service server should not panic");
}

struct RootOnly;

impl FileTree for RootOnly {
    fn attach(&mut self, _fid: Fid, _uname: &[u8], _aname: &[u8]) -> P9Result<Qid> {
        Ok(ROOT_QID)
    }

    fn walk(
        &mut self,
        _fid: Fid,
        _newfid: Fid,
        _start: Qid,
        names: &[Vec<u8>],
    ) -> P9Result<Vec<Qid>> {
        if names.is_empty() {
            Ok(Vec::new())
        } else {
            Err(P9Error::from("file does not exist"))
        }
    }

    fn open(&mut self, _fid: Fid, qid: Qid, _mode: u8) -> P9Result<OpenFile> {
        Ok(OpenFile { qid, iounit: 0 })
    }

    fn read(&mut self, _fid: Fid, _qid: Qid, _offset: u64, _count: u32) -> P9Result<ReadData> {
        Ok(ReadData::Directory(Vec::new()))
    }

    fn stat(&mut self, _qid: Qid) -> P9Result<Stat> {
        Ok(root_stat())
    }
}

struct ReferralRoot {
    endpoint: Vec<u8>,
    referral_reads: Option<Arc<AtomicUsize>>,
}

impl FileTree for ReferralRoot {
    fn attach(&mut self, _fid: Fid, _uname: &[u8], _aname: &[u8]) -> P9Result<Qid> {
        Ok(ROOT_QID)
    }

    fn walk(
        &mut self,
        _fid: Fid,
        _newfid: Fid,
        _start: Qid,
        names: &[Vec<u8>],
    ) -> P9Result<Vec<Qid>> {
        if names.is_empty() {
            Ok(Vec::new())
        } else {
            Err(P9Error::from("file does not exist"))
        }
    }

    fn open(&mut self, _fid: Fid, qid: Qid, _mode: u8) -> P9Result<OpenFile> {
        Ok(OpenFile { qid, iounit: 0 })
    }

    fn read(&mut self, _fid: Fid, _qid: Qid, _offset: u64, _count: u32) -> P9Result<ReadData> {
        Ok(ReadData::Directory(Vec::new()))
    }

    fn stat(&mut self, _qid: Qid) -> P9Result<Stat> {
        Ok(root_stat())
    }

    fn referrals(&mut self, _fid: Fid, _qid: Qid) -> P9Result<Vec<NamespaceReferral>> {
        if let Some(referral_reads) = &self.referral_reads {
            referral_reads.fetch_add(1, Ordering::SeqCst);
        }
        Ok(vec![NamespaceReferral {
            mount_path: b"/sources/x".to_vec(),
            endpoint: self.endpoint.clone(),
            uname: b"codex".to_vec(),
            aname: b"/".to_vec(),
            exported_root: b"/".to_vec(),
            authority_boundary: b"loopback".to_vec(),
            generation: 1,
            valid_for_ms: 10_000,
        }])
    }
}

struct AppearingReferralRoot {
    endpoint: Vec<u8>,
    referral_reads: u8,
}

impl FileTree for AppearingReferralRoot {
    fn attach(&mut self, _fid: Fid, _uname: &[u8], _aname: &[u8]) -> P9Result<Qid> {
        Ok(ROOT_QID)
    }

    fn walk(
        &mut self,
        _fid: Fid,
        _newfid: Fid,
        _start: Qid,
        names: &[Vec<u8>],
    ) -> P9Result<Vec<Qid>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        if names[0].as_slice() == b"sources" {
            return Ok(vec![Qid::dir(2)]);
        }
        Ok(Vec::new())
    }

    fn open(&mut self, _fid: Fid, qid: Qid, _mode: u8) -> P9Result<OpenFile> {
        Ok(OpenFile { qid, iounit: 0 })
    }

    fn read(&mut self, _fid: Fid, _qid: Qid, _offset: u64, _count: u32) -> P9Result<ReadData> {
        Ok(ReadData::Directory(Vec::new()))
    }

    fn stat(&mut self, qid: Qid) -> P9Result<Stat> {
        if qid == Qid::dir(2) {
            Ok(Stat::new(b"sources".to_vec(), qid, DMDIR | 0o555))
        } else {
            Ok(root_stat())
        }
    }

    fn referrals(&mut self, _fid: Fid, _qid: Qid) -> P9Result<Vec<NamespaceReferral>> {
        self.referral_reads = self.referral_reads.saturating_add(1);
        if self.referral_reads == 1 {
            return Ok(Vec::new());
        }
        Ok(vec![NamespaceReferral {
            mount_path: b"/sources/x".to_vec(),
            endpoint: self.endpoint.clone(),
            uname: b"codex".to_vec(),
            aname: b"/".to_vec(),
            exported_root: b"/".to_vec(),
            authority_boundary: b"loopback".to_vec(),
            generation: 1,
            valid_for_ms: 10_000,
        }])
    }
}

struct ValueTree;

impl FileTree for ValueTree {
    fn attach(&mut self, _fid: Fid, _uname: &[u8], _aname: &[u8]) -> P9Result<Qid> {
        Ok(ROOT_QID)
    }

    fn walk(
        &mut self,
        _fid: Fid,
        _newfid: Fid,
        start: Qid,
        names: &[Vec<u8>],
    ) -> P9Result<Vec<Qid>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        if start == ROOT_QID && names.len() == 1 && names[0].as_slice() == b"value" {
            Ok(vec![Qid::file(2)])
        } else {
            Err(P9Error::from("file does not exist"))
        }
    }

    fn open(&mut self, _fid: Fid, qid: Qid, _mode: u8) -> P9Result<OpenFile> {
        Ok(OpenFile { qid, iounit: 0 })
    }

    fn read(&mut self, _fid: Fid, qid: Qid, offset: u64, count: u32) -> P9Result<ReadData> {
        if qid == Qid::file(2) {
            let value = b"direct-service-value";
            let start = usize::try_from(offset)
                .unwrap_or(usize::MAX)
                .min(value.len());
            let end = start
                .saturating_add(usize::try_from(count).unwrap_or(usize::MAX))
                .min(value.len());
            Ok(ReadData::Bytes(value[start..end].to_vec()))
        } else {
            Ok(ReadData::Directory(Vec::new()))
        }
    }

    fn stat(&mut self, qid: Qid) -> P9Result<Stat> {
        if qid == Qid::file(2) {
            let mut stat = Stat::new("value", qid, 0o444);
            stat.length = u64::try_from(b"direct-service-value".len()).unwrap_or(u64::MAX);
            Ok(stat)
        } else {
            Ok(root_stat())
        }
    }
}

struct ClosingStatTree {
    shutdown_stream: Option<TcpStream>,
}

impl FileTree for ClosingStatTree {
    fn attach(&mut self, _fid: Fid, _uname: &[u8], _aname: &[u8]) -> P9Result<Qid> {
        Ok(ROOT_QID)
    }

    fn walk(
        &mut self,
        _fid: Fid,
        _newfid: Fid,
        start: Qid,
        names: &[Vec<u8>],
    ) -> P9Result<Vec<Qid>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        if start == ROOT_QID && names.len() == 1 && names[0].as_slice() == b"value" {
            Ok(vec![Qid::file(2)])
        } else {
            Err(P9Error::from("file does not exist"))
        }
    }

    fn open(&mut self, _fid: Fid, qid: Qid, _mode: u8) -> P9Result<OpenFile> {
        Ok(OpenFile { qid, iounit: 0 })
    }

    fn read(&mut self, _fid: Fid, _qid: Qid, _offset: u64, _count: u32) -> P9Result<ReadData> {
        Ok(ReadData::Directory(Vec::new()))
    }

    fn stat(&mut self, qid: Qid) -> P9Result<Stat> {
        if qid == Qid::file(2) {
            if let Some(stream) = self.shutdown_stream.take() {
                let _ = stream.shutdown(Shutdown::Both);
            }
            Ok(Stat::new("value", qid, 0o444))
        } else {
            Ok(root_stat())
        }
    }
}

fn root_stat() -> Stat {
    let mut stat = Stat::new(b".".to_vec(), ROOT_QID, DMDIR | 0o555);
    stat.uid = b"r9p".to_vec();
    stat.gid = b"r9p".to_vec();
    stat.muid = b"r9p".to_vec();
    stat
}

fn spawn_unix_root_server(socket_path: &Path) -> thread::JoinHandle<()> {
    let listener = UnixListener::bind(socket_path).expect("unix listener should bind");
    thread::spawn(move || {
        let (stream, _) = listener.accept().expect("server should accept");
        handle_connection(stream).expect("server connection should complete");
    })
}

fn handle_connection(stream: impl Read + Write) -> io::Result<()> {
    handle_configured_connection(stream, RootOnly, ServerConfig::default())
}

fn handle_configured_connection(
    mut stream: impl Read + Write,
    tree: impl FileTree,
    config: ServerConfig,
) -> io::Result<()> {
    let mut server = Server::with_config(tree, config);
    while let Some(message) = codec::read_tmessage_checked(&mut stream, server.session().msize())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?
    {
        let reply = server.handle(message);
        codec::write_rmessage_checked(&mut stream, server.session().msize(), &reply)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    }
    Ok(())
}

fn unique_socket_path(label: &str) -> PathBuf {
    env::temp_dir().join(format!("r9p-session-{label}-{}.sock", unique_id()))
}

fn unique_namespace_dir(label: &str) -> PathBuf {
    env::temp_dir().join(format!("r9p-session-{label}-{}", unique_id()))
}

fn unique_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    format!("{}-{now}", process::id())
}
