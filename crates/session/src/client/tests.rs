use super::Client;
use crate::ConnectionConfig;
use r9p::{
    codec,
    error::{Error as P9Error, Result as P9Result},
    fid::Fid,
    qid::{Qid, DMDIR},
    server::{FileTree, OpenFile, ReadData, Server},
    stat::Stat,
};
use std::{
    env, fs,
    io::{self, Read, Write},
    net::TcpListener,
    os::unix::net::UnixListener,
    path::{Path, PathBuf},
    process,
    sync::Mutex,
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
fn client_slot_replacement_bumps_session_epoch() {
    let first_socket = unique_socket_path("slot-first");
    let first_server = spawn_unix_root_server(&first_socket);
    let first_client = Client::connect_with_timeout(
        &connection(format!("unix!{}", first_socket.display())),
        Duration::ZERO,
    )
    .expect("first client should connect");
    let slot = crate::ClientSlot::new(first_client);
    let first_epoch = slot.session_epoch().expect("first epoch");

    let second_socket = unique_socket_path("slot-second");
    let second_server = spawn_unix_root_server(&second_socket);
    let second_client = Client::connect_with_timeout(
        &connection(format!("unix!{}", second_socket.display())),
        Duration::ZERO,
    )
    .expect("second client should connect");

    slot.replace(second_client).expect("replace client");
    let second_epoch = slot.session_epoch().expect("second epoch");

    assert_ne!(first_epoch, second_epoch);

    drop(slot);
    first_server.join().expect("first server should not panic");
    second_server
        .join()
        .expect("second server should not panic");
    let _ = fs::remove_file(first_socket);
    let _ = fs::remove_file(second_socket);
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

fn handle_connection(mut stream: impl Read + Write) -> io::Result<()> {
    let mut server = Server::new(RootOnly);
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
