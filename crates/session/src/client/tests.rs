use super::Client;
use crate::{ConnectionAuthentication, ConnectionConfig, ConnectionSet};
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
        authentication: ConnectionAuthentication::Unauthenticated,
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
fn bounded_path_create_sends_native_tcreate_and_publishes_the_child() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("session listener");
    let address = listener.local_addr().expect("session address");
    let value = Arc::new(Mutex::new(None));
    let server_value = Arc::clone(&value);
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("session accept");
        handle_configured_connection(
            stream,
            PublicationTree {
                value: server_value,
            },
            ServerConfig::default(),
        )
        .expect("serve publication tree");
    });

    let client =
        Client::connect_with_timeout(&connection(address.to_string()), Duration::from_secs(1))
            .expect("namespace client should connect");
    let qid = client
        .create_at_timeout("/", "published", 0o600, r9p::OWRITE, Duration::from_secs(1))
        .expect("bounded native create should succeed");
    assert_eq!(qid, PUBLICATION_QID);
    assert!(value
        .lock()
        .expect("publication value lock")
        .as_ref()
        .is_some());

    client.shutdown().expect("namespace should shut down");
    drop(client);
    server.join().expect("server should not panic");
}

#[test]
fn bounded_reconcile_handles_a_missing_or_existing_file_without_a_stat_preflight() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("session listener");
    let address = listener.local_addr().expect("session address");
    let value = Arc::new(Mutex::new(None));
    let server_value = Arc::clone(&value);
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("server should accept");
        handle_configured_connection(
            stream,
            PublicationTree {
                value: server_value,
            },
            ServerConfig::default(),
        )
        .expect("server connection should complete");
    });
    let client =
        Client::connect_with_timeout(&connection(address.to_string()), Duration::from_secs(2))
            .expect("client should connect");

    assert_eq!(
        client
            .reconcile_file_at_timeout("/", "published", 0o666, b"first", Duration::from_secs(1),)
            .expect("missing file should be created"),
        5
    );
    assert_eq!(
        client
            .reconcile_file_at_timeout("/", "published", 0o666, b"second", Duration::from_secs(1),)
            .expect("existing file should be replaced"),
        6
    );
    assert_eq!(
        client
            .read_path("/published")
            .expect("published value should remain readable"),
        b"second"
    );

    client.shutdown().expect("session should shut down");
    drop(client);
    server.join().expect("server should not panic");
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
fn client_session_moves_to_the_next_endpoint_after_transport_failure() {
    let primary_listener = TcpListener::bind("127.0.0.1:0").expect("primary listener");
    let primary_address = primary_listener.local_addr().expect("primary address");
    let primary = thread::spawn(move || {
        let (stream, _) = primary_listener.accept().expect("primary accept");
        handle_connection(stream).expect("primary connection");
    });

    let fallback_listener = TcpListener::bind("127.0.0.1:0").expect("fallback listener");
    let fallback_address = fallback_listener.local_addr().expect("fallback address");
    let fallback = thread::spawn(move || {
        let (stream, _) = fallback_listener.accept().expect("fallback accept");
        handle_connection(stream).expect("fallback connection");
    });

    let connections = ConnectionSet::new(vec![
        connection(primary_address.to_string()),
        connection(fallback_address.to_string()),
    ])
    .expect("equivalent endpoint set");
    let session = crate::ClientSession::connect_set(&connections, Duration::from_millis(250))
        .expect("primary should connect");
    assert_eq!(session.active_address(), primary_address.to_string());
    assert_eq!(
        session.candidate_addresses(),
        vec![primary_address.to_string(), fallback_address.to_string()]
    );

    let first = session.snapshot().expect("primary attachment");
    let replacement = session
        .reconnect_after(&first)
        .expect("failed primary should move to fallback");
    assert_eq!(session.active_address(), fallback_address.to_string());
    replacement
        .stat_timeout(replacement.root_fid(), Duration::from_secs(1))
        .expect("fallback attachment should serve the namespace");

    session.shutdown().expect("fallback should shut down");
    first.shutdown().expect("primary should shut down");
    drop(replacement);
    drop(first);
    drop(session);
    primary.join().expect("primary server should not panic");
    fallback.join().expect("fallback server should not panic");
}

#[test]
fn client_session_uses_a_fallback_when_the_primary_is_unreachable() {
    let reservation = TcpListener::bind("127.0.0.1:0").expect("reserve primary address");
    let primary_address = reservation.local_addr().expect("primary address");
    drop(reservation);

    let fallback_listener = TcpListener::bind("127.0.0.1:0").expect("fallback listener");
    let fallback_address = fallback_listener.local_addr().expect("fallback address");
    let fallback = thread::spawn(move || {
        let (stream, _) = fallback_listener.accept().expect("fallback accept");
        handle_connection(stream).expect("fallback connection");
    });

    let connections = ConnectionSet::new(vec![
        connection(primary_address.to_string()),
        connection(fallback_address.to_string()),
    ])
    .expect("equivalent endpoint set");
    let session = crate::ClientSession::connect_set(&connections, Duration::from_millis(100))
        .expect("fallback should connect");
    assert_eq!(session.active_address(), fallback_address.to_string());

    session.shutdown().expect("fallback should shut down");
    drop(session);
    fallback.join().expect("fallback server should not panic");
}

#[test]
fn prepared_client_session_adopts_the_initial_attachment_without_reconnecting() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("session listener");
    let address = listener.local_addr().expect("session address");
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("session accept");
        handle_connection(stream).expect("session connection");
    });

    let prepared = crate::PreparedClientSession::connect(
        &connection(address.to_string()),
        Duration::from_secs(1),
    )
    .expect("prepared session should connect");
    let initial = prepared.client().clone();
    initial
        .stat_timeout(initial.root_fid(), Duration::from_secs(1))
        .expect("initial operation should use the prepared attachment");

    let session = prepared.into_session();
    let adopted = session.snapshot().expect("adopted attachment");
    assert!(adopted.same_session(&initial));
    adopted
        .stat_timeout(adopted.root_fid(), Duration::from_secs(1))
        .expect("adopted attachment should remain usable");

    session.shutdown().expect("session should shut down");
    drop(adopted);
    drop(initial);
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

const PUBLICATION_QID: Qid = Qid::file(2);

struct PublicationTree {
    value: Arc<Mutex<Option<Vec<u8>>>>,
}

impl FileTree for PublicationTree {
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
        let present = self.value.lock().expect("publication value lock").is_some();
        if start == ROOT_QID && names.len() == 1 && names[0].as_slice() == b"published" && present {
            Ok(vec![PUBLICATION_QID])
        } else {
            Err(P9Error::from_static(r9p::error::ENOENT))
        }
    }

    fn open(&mut self, _fid: Fid, qid: Qid, mode: u8) -> P9Result<OpenFile> {
        if qid == PUBLICATION_QID && mode & r9p::OTRUNC != 0 {
            self.value
                .lock()
                .expect("publication value lock")
                .as_mut()
                .expect("opened publication should exist")
                .clear();
        }
        Ok(OpenFile { qid, iounit: 0 })
    }

    fn create(
        &mut self,
        _fid: Fid,
        qid: Qid,
        name: &[u8],
        _perm: u32,
        _mode: u8,
    ) -> P9Result<OpenFile> {
        if qid != ROOT_QID || name != b"published" {
            return Err(P9Error::from_static(r9p::error::ENOENT));
        }
        let mut value = self.value.lock().expect("publication value lock");
        if value.is_some() {
            return Err(P9Error::from_static(r9p::error::EEXIST));
        }
        *value = Some(Vec::new());
        Ok(OpenFile {
            qid: PUBLICATION_QID,
            iounit: 0,
        })
    }

    fn read(&mut self, _fid: Fid, qid: Qid, offset: u64, count: u32) -> P9Result<ReadData> {
        if qid == ROOT_QID {
            return Ok(ReadData::Directory(Vec::new()));
        }
        let value = self.value.lock().expect("publication value lock");
        let bytes = value.as_ref().expect("read publication should exist");
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        let end = start.saturating_add(count as usize).min(bytes.len());
        Ok(ReadData::Bytes(bytes[start..end].to_vec()))
    }

    fn write(&mut self, _fid: Fid, qid: Qid, offset: u64, data: &[u8]) -> P9Result<u32> {
        if qid != PUBLICATION_QID || offset != 0 {
            return Err(P9Error::from("invalid publication write"));
        }
        let mut value = self.value.lock().expect("publication value lock");
        *value.as_mut().expect("write publication should exist") = data.to_vec();
        Ok(u32::try_from(data.len()).expect("test data should fit"))
    }

    fn stat(&mut self, qid: Qid) -> P9Result<Stat> {
        if qid == ROOT_QID {
            return Ok(root_stat());
        }
        let mut stat = Stat::new("published", PUBLICATION_QID, 0o666);
        stat.length = self
            .value
            .lock()
            .expect("publication value lock")
            .as_ref()
            .expect("stat publication should exist")
            .len() as u64;
        Ok(stat)
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
