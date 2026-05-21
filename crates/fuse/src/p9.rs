use crate::error::{p9_error, Error, Result};
use r9p::{
    blocking,
    fid::Fid,
    multiplex::{MultiplexTransport, MultiplexedClient},
    qid::Qid,
    stat::Stat,
};
use std::{
    env,
    io::{self, Read, Write},
    net::{Shutdown, TcpStream},
    path::{Path, PathBuf},
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::net::UnixStream;

pub const OREAD: u8 = blocking::OREAD;
pub const OWRITE: u8 = blocking::OWRITE;
pub const ORDWR: u8 = blocking::ORDWR;
pub const OTRUNC: u8 = blocking::OTRUNC;

#[derive(Clone)]
pub struct Client {
    inner: MultiplexedClient<ClientStream>,
}

impl Client {
    pub fn connect(address: &str, uname: &str, aname: &str, msize: u32) -> Result<Self> {
        let stream = connect_stream(address)?;
        let inner =
            MultiplexedClient::connect(stream, uname, aname, msize).map_err(client_error)?;
        Ok(Self { inner })
    }

    pub fn root_fid(&self) -> Fid {
        self.inner.root_fid()
    }

    pub fn clone_fid(&self, fid: Fid) -> Result<Fid> {
        self.walk(fid, &[])
    }

    pub fn clone_fid_timeout(&self, fid: Fid, timeout: Duration) -> Result<Fid> {
        self.walk_timeout(fid, &[], timeout)
    }

    pub fn walk_one(&self, fid: Fid, name: &[u8]) -> Result<Fid> {
        self.walk(fid, &[name.to_vec()])
    }

    pub fn walk_one_timeout(&self, fid: Fid, name: &[u8], timeout: Duration) -> Result<Fid> {
        self.walk_timeout(fid, &[name.to_vec()], timeout)
    }

    pub fn walk(&self, fid: Fid, names: &[Vec<u8>]) -> Result<Fid> {
        self.inner.walk(fid, names).map_err(client_error)
    }

    pub fn walk_timeout(&self, fid: Fid, names: &[Vec<u8>], timeout: Duration) -> Result<Fid> {
        self.inner
            .walk_timeout(fid, names, timeout)
            .map_err(client_error)
    }

    pub fn open(&self, fid: Fid, mode: u8) -> Result<Qid> {
        self.inner.open(fid, mode).map_err(client_error)
    }

    pub fn open_timeout(&self, fid: Fid, mode: u8, timeout: Duration) -> Result<Qid> {
        self.inner
            .open_timeout(fid, mode, timeout)
            .map_err(client_error)
    }

    pub fn create_timeout(
        &self,
        parent_fid: Fid,
        name: &[u8],
        perm: u32,
        mode: u8,
        timeout: Duration,
    ) -> Result<(Fid, Qid)> {
        self.inner
            .create_timeout(parent_fid, name, perm, mode, timeout)
            .map_err(client_error)
    }

    pub fn read_timeout(
        &self,
        fid: Fid,
        offset: u64,
        count: u32,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        self.inner
            .read_timeout(fid, offset, count, timeout)
            .map_err(client_error)
    }

    pub fn read_full_timeout(
        &self,
        fid: Fid,
        offset: u64,
        count: u32,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        self.inner
            .read_full_timeout(fid, offset, count, timeout)
            .map_err(client_error)
    }

    pub fn write_timeout(
        &self,
        fid: Fid,
        offset: u64,
        data: &[u8],
        timeout: Duration,
    ) -> Result<u32> {
        self.inner
            .write_timeout(fid, offset, data, timeout)
            .map_err(client_error)
    }

    pub fn clunk(&self, fid: Fid) -> Result<()> {
        self.inner.clunk(fid).map_err(client_error)
    }

    pub fn clunk_timeout(&self, fid: Fid, timeout: Duration) -> Result<()> {
        self.inner.clunk_timeout(fid, timeout).map_err(client_error)
    }

    pub fn remove(&self, fid: Fid) -> Result<()> {
        self.inner.remove(fid).map_err(client_error)
    }

    pub fn stat(&self, fid: Fid) -> Result<Stat> {
        self.inner.stat(fid).map_err(client_error)
    }

    pub fn stat_timeout(&self, fid: Fid, timeout: Duration) -> Result<Stat> {
        self.inner.stat_timeout(fid, timeout).map_err(client_error)
    }

    pub fn wstat(&self, fid: Fid, stat: Stat) -> Result<()> {
        self.inner.wstat(fid, stat).map_err(client_error)
    }
}

enum ClientStream {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(UnixStream),
}

impl Read for ClientStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.read(buffer),
            #[cfg(unix)]
            Self::Unix(stream) => stream.read(buffer),
        }
    }
}

impl Write for ClientStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.write(buffer),
            #[cfg(unix)]
            Self::Unix(stream) => stream.write(buffer),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.flush(),
            #[cfg(unix)]
            Self::Unix(stream) => stream.flush(),
        }
    }
}

impl MultiplexTransport for ClientStream {
    fn try_clone_transport(&self) -> io::Result<Self> {
        match self {
            Self::Tcp(stream) => stream.try_clone().map(Self::Tcp),
            #[cfg(unix)]
            Self::Unix(stream) => stream.try_clone().map(Self::Unix),
        }
    }

    fn shutdown_transport(&self) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.shutdown(Shutdown::Both),
            #[cfg(unix)]
            Self::Unix(stream) => stream.shutdown(Shutdown::Both),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ConnectTarget {
    Tcp(String),
    Unix(PathBuf),
}

fn connect_stream(address: &str) -> Result<ClientStream> {
    match parse_connection_target(address)? {
        ConnectTarget::Tcp(socket) => {
            let stream = TcpStream::connect(&socket)
                .map_err(|error| Error::io(format!("connect {socket}"), error))?;
            stream
                .set_nodelay(true)
                .map_err(|error| Error::io("set TCP_NODELAY", error))?;
            Ok(ClientStream::Tcp(stream))
        }
        ConnectTarget::Unix(path) => connect_unix_stream(&path),
    }
}

fn parse_connection_target(address: &str) -> Result<ConnectTarget> {
    if let Some(path) = address.strip_prefix("unix!") {
        return parse_unix_target(path);
    }
    if let Some(service) = address.strip_prefix("namespace!") {
        let namespace = env::var("NAMESPACE")
            .map_err(|_| Error::new(libc::EINVAL, "NAMESPACE is required for namespace!"))?;
        return namespace_service_path(Path::new(&namespace), service).map(ConnectTarget::Unix);
    }
    parse_tcp_address(address).map(ConnectTarget::Tcp)
}

fn parse_unix_target(path: &str) -> Result<ConnectTarget> {
    if path.is_empty() {
        return Err(Error::new(libc::EINVAL, "unix! address requires a path"));
    }
    Ok(ConnectTarget::Unix(PathBuf::from(path)))
}

fn namespace_service_path(namespace: &Path, service: &str) -> Result<PathBuf> {
    if service.is_empty() {
        return Err(Error::new(
            libc::EINVAL,
            "namespace! address requires a service",
        ));
    }
    if service.contains('/') {
        return Err(Error::new(
            libc::EINVAL,
            "namespace! service must be a single path element",
        ));
    }
    if namespace.as_os_str().is_empty() {
        return Err(Error::new(libc::EINVAL, "NAMESPACE must not be empty"));
    }
    Ok(namespace.join(service))
}

#[cfg(unix)]
fn connect_unix_stream(path: &Path) -> Result<ClientStream> {
    UnixStream::connect(path)
        .map(ClientStream::Unix)
        .map_err(|error| Error::io(format!("connect {}", path.display()), error))
}

#[cfg(not(unix))]
fn connect_unix_stream(path: &Path) -> Result<ClientStream> {
    Err(Error::new(
        libc::ENOSYS,
        format!(
            "unix sockets are not supported on this platform: {}",
            path.display()
        ),
    ))
}

pub fn parse_tcp_address(address: &str) -> Result<String> {
    blocking::parse_tcp_address(address)
        .map_err(|error| Error::new(libc::EINVAL, error.display_lossy().to_string()))
}

fn client_error(error: r9p::Error) -> Error {
    let message = error.display_lossy().to_string();
    if is_protocol_error(&message) {
        Error::new(libc::EPROTO, format!("9P client state: {message}"))
    } else if is_transport_message(&message) {
        Error::new(
            transport_errno(&message).unwrap_or(libc::EIO),
            format!("9P client state: {message}"),
        )
    } else {
        p9_error(error.message())
    }
}

fn is_protocol_error(message: &str) -> bool {
    message.starts_with("9P client state:")
        || message.starts_with("response tag mismatch")
        || message.starts_with("unknown response")
        || message.starts_with("duplicate waiter")
        || message.starts_with("multiplexed calls require")
}

fn is_transport_message(message: &str) -> bool {
    message.contains("9P frame")
        || message.contains("9P reader stopped")
        || message.contains("9P response timeout")
        || message.contains("clone 9P stream")
        || message.contains("lock 9P")
}

fn transport_errno(message: &str) -> Option<i32> {
    if message.contains("9P reader stopped") {
        return Some(libc::ENOTCONN);
    }
    if message.contains("9P response timeout") {
        return Some(libc::ETIMEDOUT);
    }
    let marker = "os error ";
    let start = message.find(marker)? + marker.len();
    let rest = &message[start..];
    let digits = rest
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .collect::<String>();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::{
        client_error, namespace_service_path, parse_connection_target, parse_tcp_address, Client,
    };
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
    fn parses_plan9port_tcp_address() {
        let parsed = parse_tcp_address("tcp!127.0.0.1!19564").expect("address should parse");
        assert_eq!(parsed, "127.0.0.1:19564");
    }

    #[test]
    fn defaults_bare_host_to_9p_port() {
        let parsed = parse_tcp_address("vault.local").expect("address should parse");
        assert_eq!(parsed, "vault.local:564");
    }

    #[test]
    fn parses_unix_address() {
        let parsed = parse_connection_target("unix!/tmp/r9p.sock").expect("address should parse");
        assert_eq!(parsed, super::ConnectTarget::Unix("/tmp/r9p.sock".into()));
    }

    #[test]
    fn closed_multiplex_reader_maps_to_transport_errno() {
        let error = client_error(P9Error::from("9P reader stopped before response"));
        assert_eq!(error.errno, libc::ENOTCONN);
        assert!(error.message().contains("9P reader stopped"));
    }

    #[test]
    fn resolves_namespace_service_under_namespace_dir() {
        let path = namespace_service_path(Path::new("/tmp/namespace"), "runtime-recovery")
            .expect("service should resolve");
        assert_eq!(path, PathBuf::from("/tmp/namespace/runtime-recovery"));
    }

    #[test]
    fn rejects_namespace_service_paths() {
        let error = namespace_service_path(Path::new("/tmp/namespace"), "runtime/recovery")
            .expect_err("service path should be rejected");
        assert_eq!(error.errno, libc::EINVAL);
        assert!(error.message().contains("single path element"));
    }

    #[test]
    fn connects_explicit_unix_socket() {
        let socket_path = unique_socket_path("explicit");
        let server = spawn_unix_root_server(&socket_path);
        let client = Client::connect(
            &format!("unix!{}", socket_path.display()),
            "codex",
            "/",
            8192,
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
    fn connects_namespace_socket() {
        let _env = ENV_LOCK.lock().expect("env lock should not be poisoned");
        let namespace = unique_namespace_dir("namespace");
        fs::create_dir_all(&namespace).expect("namespace dir should be created");
        let socket_path = namespace.join("runtime-recovery");
        let previous = env::var("NAMESPACE").ok();
        env::set_var("NAMESPACE", &namespace);
        let server = spawn_unix_root_server(&socket_path);

        let client = Client::connect("namespace!runtime-recovery", "codex", "/", 8192)
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
        stat.uid = b"r9pfuse".to_vec();
        stat.gid = b"r9pfuse".to_vec();
        stat.muid = b"r9pfuse".to_vec();
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
        env::temp_dir().join(format!("r9pfuse-p9-{label}-{}.sock", unique_id()))
    }

    fn unique_namespace_dir(label: &str) -> PathBuf {
        env::temp_dir().join(format!("r9pfuse-p9-{label}-{}", unique_id()))
    }

    fn unique_id() -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        format!("{}-{now}", process::id())
    }
}
