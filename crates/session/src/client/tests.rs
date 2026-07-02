use super::Client;
use r9p::{
    codec,
    error::{Error as P9Error, Result as P9Result},
    fid::Fid,
    message::TMessage,
    qid::{Qid, DMDIR},
    server::{FileTree, OpenFile, ReadData, Server},
    stat::Stat,
};
use std::{
    env, fs,
    io::{self, Read, Write},
    os::unix::net::UnixListener,
    path::{Path, PathBuf},
    process,
    sync::Mutex,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

static ENV_LOCK: Mutex<()> = Mutex::new(());
const ROOT_QID: Qid = Qid::dir(1);

#[test]
fn connects_explicit_unix_socket() {
    let socket_path = unique_socket_path("explicit");
    let server = spawn_unix_root_server(&socket_path);
    let client = Client::connect_with_timeout(
        &format!("unix!{}", socket_path.display()),
        "codex",
        "/",
        8192,
        Duration::ZERO,
    )
    .expect("client should connect");

    let stat = client
        .stat_timeout(client.root_fid(), Duration::from_secs(1))
        .expect("root stat should succeed");
    assert_eq!(stat.name, b".".to_vec());

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
        &format!("unix!{}", socket_path.display()),
        "codex",
        "/",
        8192,
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
fn connects_namespace_socket() {
    let _env = ENV_LOCK.lock().expect("env lock should not be poisoned");
    let namespace = unique_namespace_dir("namespace");
    fs::create_dir_all(&namespace).expect("namespace dir should be created");
    let socket_path = namespace.join("example-service");
    let previous = env::var("NAMESPACE").ok();
    env::set_var("NAMESPACE", &namespace);
    let server = spawn_unix_root_server(&socket_path);

    let client = Client::connect_with_timeout(
        "namespace!example-service",
        "codex",
        "/",
        8192,
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
    while let Some(message) = read_tmessage(&mut stream)? {
        let reply = server.handle(message);
        let frame = codec::encode_rmessage_checked(&reply, server.session().msize())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        stream.write_all(&frame)?;
    }
    Ok(())
}

fn read_tmessage(stream: &mut impl Read) -> io::Result<Option<TMessage>> {
    let mut prefix = [0_u8; 4];
    match stream.read_exact(&mut prefix) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let size = u32::from_le_bytes(prefix);
    if size < codec::FRAME_HEADER_SIZE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "short 9P frame"));
    }
    let frame_len = usize::try_from(size)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "oversized 9P frame"))?;
    let mut frame = vec![0_u8; frame_len];
    frame[..4].copy_from_slice(&prefix);
    stream.read_exact(&mut frame[4..])?;
    codec::decode_tmessage(&frame)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))
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
