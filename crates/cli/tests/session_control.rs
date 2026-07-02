use std::{
    error::Error,
    fs, io,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use r9p::{
    codec,
    fid::Fid,
    message::TMessage,
    qid::{Qid, DMDIR},
    server::{FileTree, OpenFile, ReadData, Server},
    stat::Stat,
    Error as R9pError, Result as R9pResult,
};

type TestResult<T> = Result<T, Box<dyn Error>>;

#[test]
fn session_control_verbs_use_local_socket() -> TestResult<()> {
    let (address, server) = start_server()?;
    let socket = temp_path("r9p-session-control.sock");
    let socket_arg = socket.to_string_lossy().into_owned();

    let mut session = ChildGuard::spawn(
        Command::new(r9p_bin())
            .arg("-a")
            .arg(&address)
            .arg("-u")
            .arg("test")
            .arg("-A")
            .arg("/")
            .arg("-m")
            .arg("8192")
            .arg("session")
            .arg("serve")
            .arg("--socket")
            .arg(&socket_arg)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )?;

    let status = run_session_until_success(&["session", "status", "--socket", &socket_arg])?;
    assert_stdout_contains(&status, "\"kind\":\"session.status.v1\"")?;
    assert_stdout_contains(&status, "\"attached\":true")?;

    let direct_status =
        run_session_until_success(&["-a", &format!("unix!{socket_arg}"), "read", "/status"])?;
    assert_stdout_contains(&direct_status, "\"kind\":\"session.status.v1\"")?;

    let snapshot = run_session_until_success(&[
        "session",
        "snapshot",
        "--socket",
        &socket_arg,
        "--depth",
        "1",
        "/",
    ])?;
    assert_stdout_contains(&snapshot, "\"kind\":\"session.snapshot.v1\"")?;
    assert_stdout_contains(&snapshot, "\"path\":\"/data\"")?;
    assert_stdout_contains(&snapshot, "\"path\":\"/docs\"")?;

    let stat = run_session_until_success(&["session", "stat", "--socket", &socket_arg, "/data"])?;
    assert_stdout_contains(&stat, "\"kind\":\"session.stat.v1\"")?;
    assert_stdout_contains(&stat, "\"path\":\"/data\"")?;
    assert_stdout_contains(&stat, "\"length\":6")?;

    let list = run_session_until_success(&["session", "list", "--socket", &socket_arg, "/"])?;
    assert_stdout_contains(&list, "\"kind\":\"session.list.v1\"")?;
    assert_stdout_contains(&list, "\"path\":\"/docs\"")?;

    let read = run_session_until_success(&["session", "read", "--socket", &socket_arg, "/data"])?;
    assert_stdout_contains(&read, "\"kind\":\"session.read.v1\"")?;
    assert_stdout_contains(&read, "\"bytes\":6")?;
    assert_stdout_contains(&read, "\"data_hex\":\"68656c6c6f0a\"")?;

    session.stop();
    let _ = fs::remove_file(socket);
    join_server(server)
}

struct SessionTree;

impl FileTree for SessionTree {
    fn attach(&mut self, _fid: Fid, _uname: &[u8], _aname: &[u8]) -> R9pResult<Qid> {
        Ok(Qid::dir(1))
    }

    fn walk(
        &mut self,
        _fid: Fid,
        _newfid: Fid,
        start: Qid,
        names: &[Vec<u8>],
    ) -> R9pResult<Vec<Qid>> {
        match names {
            [] => Ok(Vec::new()),
            [name] if start == Qid::dir(1) && name == b"data" => Ok(vec![Qid::file(2)]),
            [name] if start == Qid::dir(1) && name == b"docs" => Ok(vec![Qid::dir(3)]),
            _ => Ok(Vec::new()),
        }
    }

    fn open(&mut self, _fid: Fid, qid: Qid, _mode: u8) -> R9pResult<OpenFile> {
        Ok(OpenFile { qid, iounit: 0 })
    }

    fn read(&mut self, _fid: Fid, qid: Qid, offset: u64, count: u32) -> R9pResult<ReadData> {
        if qid == Qid::dir(1) {
            return Ok(ReadData::Directory(vec![
                Stat::new("data", Qid::file(2), 0o444),
                Stat::new("docs", Qid::dir(3), DMDIR | 0o555),
            ]));
        }
        if qid == Qid::dir(3) {
            return Ok(ReadData::Directory(Vec::new()));
        }
        slice_bytes(b"hello\n", offset, count)
    }

    fn stat(&mut self, qid: Qid) -> R9pResult<Stat> {
        if qid == Qid::file(2) {
            let mut stat = Stat::new("data", qid, 0o444);
            stat.length = 6;
            Ok(stat)
        } else if qid == Qid::dir(3) {
            Ok(Stat::new("docs", qid, DMDIR | 0o555))
        } else {
            Ok(Stat::new(".", qid, DMDIR | 0o555))
        }
    }
}

fn slice_bytes(data: &[u8], offset: u64, count: u32) -> R9pResult<ReadData> {
    let start = usize::try_from(offset)
        .map_err(|_| R9pError::from("read offset too large"))?
        .min(data.len());
    let end = start
        .saturating_add(usize::try_from(count).unwrap_or(usize::MAX))
        .min(data.len());
    Ok(ReadData::Bytes(data[start..end].to_vec()))
}

fn start_server() -> TestResult<(String, JoinHandle<Result<(), String>>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?.to_string();
    let handle = thread::spawn(move || -> Result<(), String> {
        let (stream, _) = listener.accept().map_err(|error| error.to_string())?;
        serve_connection(stream)
    });
    Ok((address, handle))
}

fn serve_connection(mut stream: TcpStream) -> Result<(), String> {
    let mut server = Server::new(SessionTree);
    while let Some(message) = read_tmessage(&mut stream)? {
        let reply = server.handle(message);
        let frame = codec::encode_rmessage_checked(&reply, server.session().msize())
            .map_err(|error| error.to_string())?;
        stream
            .write_all(&frame)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn read_tmessage(stream: &mut impl Read) -> Result<Option<TMessage>, String> {
    let mut prefix = [0_u8; 4];
    match stream.read_exact(&mut prefix) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error.to_string()),
    }
    let size = u32::from_le_bytes(prefix);
    if size < codec::FRAME_HEADER_SIZE {
        return Err("short 9P frame".to_string());
    }
    let frame_len = usize::try_from(size).map_err(|_| "oversized 9P frame".to_string())?;
    let mut frame = vec![0_u8; frame_len];
    frame[..4].copy_from_slice(&prefix);
    stream
        .read_exact(&mut frame[4..])
        .map_err(|error| error.to_string())?;
    codec::decode_tmessage(&frame)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn run_session_until_success(args: &[&str]) -> TestResult<Output> {
    let mut last = None;
    for _ in 0..50 {
        let output = run_r9p(args)?;
        if output.status.success() {
            return Ok(output);
        }
        last = Some(output);
        thread::sleep(Duration::from_millis(20));
    }
    let output = last.ok_or_else(|| test_error("session command did not run"))?;
    Err(test_error(format!(
        "session command failed stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )))
}

fn run_r9p(args: &[&str]) -> TestResult<Output> {
    Ok(Command::new(r9p_bin())
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?)
}

fn assert_stdout_contains(output: &Output, needle: &str) -> TestResult<()> {
    if String::from_utf8_lossy(&output.stdout).contains(needle) {
        Ok(())
    } else {
        Err(test_error(format!(
            "stdout did not contain {needle}: {}",
            String::from_utf8_lossy(&output.stdout)
        )))
    }
}

struct ChildGuard {
    child: Option<Child>,
}

impl ChildGuard {
    fn spawn(command: &mut Command) -> io::Result<Self> {
        command.spawn().map(|child| Self { child: Some(child) })
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.stop();
    }
}

fn join_server(handle: JoinHandle<Result<(), String>>) -> TestResult<()> {
    match handle.join() {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(test_error(error)),
        Err(_) => Err(test_error("server thread panicked")),
    }
}

fn r9p_bin() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_r9p")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/debug/r9p"))
}

fn temp_path(label: &str) -> PathBuf {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("{label}-{}-{now}", std::process::id()))
}

fn test_error(message: impl Into<String>) -> Box<dyn Error> {
    Box::new(io::Error::other(message.into()))
}
