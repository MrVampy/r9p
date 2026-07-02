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
            .arg("--change-feed")
            .arg("/events/namespace/recent")
            .arg("--change-feed-stream")
            .arg("/events/namespace/stream")
            .arg("--change-feed-poll-interval")
            .arg("0.01")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )?;

    let status = run_session_until_success(&["session", "status", "--socket", &socket_arg])?;
    assert_stdout_contains(&status, "\"kind\":\"session.status.v1\"")?;
    assert_stdout_contains(&status, "\"attached\":true")?;

    let feed_status = run_session_until_stdout_contains(
        &["session", "status", "--socket", &socket_arg],
        "\"last_event_id\":\"e1\"",
    )?;
    assert_stdout_contains(&feed_status, "\"state\":\"connected\"")?;
    assert_stdout_contains(&feed_status, "\"source\":\"stream\"")?;
    assert_stdout_contains(&feed_status, "\"session_epoch\":\"session:")?;
    assert_stdout_contains(&feed_status, "\"last_generation\":42")?;

    let direct_status =
        run_session_until_success(&["-a", &format!("unix!{socket_arg}"), "read", "/status"])?;
    assert_stdout_contains(&direct_status, "\"kind\":\"session.status.v1\"")?;

    let direct_query = run_session_until_success(&[
        "-a",
        &format!("unix!{socket_arg}"),
        "rpc",
        "/query",
        r#"{"op":"stat","path":"/data"}"#,
    ])?;
    assert_stdout_contains(&direct_query, "\"kind\":\"session.stat.v1\"")?;
    assert_stdout_contains(&direct_query, "\"path\":\"/data\"")?;

    let filtered_query = run_session_until_success(&[
        "-a",
        &format!("unix!{socket_arg}"),
        "rpc",
        "/query",
        r#"{"op":"snapshot","path":"/","depth":1,"include":"files","fields":["path","kind","length"],"budget":1}"#,
    ])?;
    assert_stdout_contains(&filtered_query, "\"kind\":\"session.snapshot.v1\"")?;
    assert_stdout_contains(&filtered_query, "\"path\":\"/data\"")?;
    assert_stdout_contains(&filtered_query, "\"kind\":\"file\"")?;
    assert_stdout_contains(&filtered_query, "\"length\":6")?;
    assert_stdout_contains(&filtered_query, "\"feed_generation\":42")?;
    assert_stdout_contains(&filtered_query, "\"fresh_instance\":false")?;
    assert_stdout_contains(&filtered_query, "\"reason\":\"budget_truncated\"")?;

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
    assert_stdout_contains(&snapshot, "\"cache\":{\"enabled\":true")?;
    assert_stdout_contains(&snapshot, "\"dir_hits\":1")?;
    assert_stdout_contains(&snapshot, "\"path\":\"/data\"")?;
    assert_stdout_contains(&snapshot, "\"path\":\"/docs\"")?;
    assert_stdout_contains(&snapshot, "\"path\":\"/denied\"")?;

    let denied_snapshot = run_session_until_success(&[
        "session",
        "snapshot",
        "--socket",
        &socket_arg,
        "--depth",
        "2",
        "/",
    ])?;
    assert_stdout_contains(&denied_snapshot, "\"path\":\"/denied\"")?;
    assert_stdout_contains(&denied_snapshot, "\"reason\":\"denied\"")?;

    let revalidated_snapshot = run_session_until_success(&[
        "-a",
        &format!("unix!{socket_arg}"),
        "rpc",
        "/query",
        r#"{"op":"snapshot","path":"/","depth":1,"freshness":"must_revalidate","fields":["path"]}"#,
    ])?;
    assert_stdout_contains(&revalidated_snapshot, "\"cache\":{\"enabled\":false")?;
    assert_stdout_contains(&revalidated_snapshot, "\"stat_hits\":0")?;

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

const NAMESPACE_CHANGE: &[u8] =
    b"namespace_change\tevent_id=e1\tgeneration=42\tscope=shared\tchange_kind=modified\tpath=/docs/changed\n";

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
            [name] if start == Qid::dir(1) && name == b"events" => Ok(vec![Qid::dir(5)]),
            [parent, child]
                if start == Qid::dir(1) && parent == b"events" && child == b"namespace" =>
            {
                Ok(vec![Qid::dir(5), Qid::dir(6)])
            }
            [grandparent, parent, child]
                if start == Qid::dir(1)
                    && grandparent == b"events"
                    && parent == b"namespace"
                    && child == b"recent" =>
            {
                Ok(vec![Qid::dir(5), Qid::dir(6), Qid::file(7)])
            }
            [grandparent, parent, child]
                if start == Qid::dir(1)
                    && grandparent == b"events"
                    && parent == b"namespace"
                    && child == b"stream" =>
            {
                Ok(vec![Qid::dir(5), Qid::dir(6), Qid::file(8)])
            }
            [name] if start == Qid::dir(5) && name == b"namespace" => Ok(vec![Qid::dir(6)]),
            [name] if start == Qid::dir(6) && name == b"recent" => Ok(vec![Qid::file(7)]),
            [name] if start == Qid::dir(6) && name == b"stream" => Ok(vec![Qid::file(8)]),
            [name] if start == Qid::dir(1) && name == b"denied" => {
                Err(R9pError::from("permission denied"))
            }
            _ => Ok(Vec::new()),
        }
    }

    fn open(&mut self, _fid: Fid, qid: Qid, _mode: u8) -> R9pResult<OpenFile> {
        Ok(OpenFile { qid, iounit: 0 })
    }

    fn read(&mut self, _fid: Fid, qid: Qid, offset: u64, count: u32) -> R9pResult<ReadData> {
        if qid == Qid::dir(1) {
            return Ok(ReadData::Directory(vec![
                data_stat(),
                Stat::new("docs", Qid::dir(3), DMDIR | 0o555),
                Stat::new("denied", Qid::dir(4), DMDIR | 0o555),
                Stat::new("events", Qid::dir(5), DMDIR | 0o555),
            ]));
        }
        if qid == Qid::dir(3) {
            return Ok(ReadData::Directory(Vec::new()));
        }
        if qid == Qid::dir(5) {
            return Ok(ReadData::Directory(vec![Stat::new(
                "namespace",
                Qid::dir(6),
                DMDIR | 0o555,
            )]));
        }
        if qid == Qid::dir(6) {
            return Ok(ReadData::Directory(vec![
                Stat::new("recent", Qid::file(7), 0o444),
                Stat::new("stream", Qid::file(8), 0o444),
            ]));
        }
        if qid == Qid::file(7) || qid == Qid::file(8) {
            return slice_bytes(NAMESPACE_CHANGE, offset, count);
        }
        slice_bytes(b"hello\n", offset, count)
    }

    fn stat(&mut self, qid: Qid) -> R9pResult<Stat> {
        if qid == Qid::file(2) {
            Ok(data_stat())
        } else if qid == Qid::dir(3) {
            Ok(Stat::new("docs", qid, DMDIR | 0o555))
        } else if qid == Qid::dir(5) {
            Ok(Stat::new("events", qid, DMDIR | 0o555))
        } else if qid == Qid::dir(6) {
            Ok(Stat::new("namespace", qid, DMDIR | 0o555))
        } else if qid == Qid::file(7) {
            let mut stat = Stat::new("recent", qid, 0o444);
            stat.length = u64::try_from(NAMESPACE_CHANGE.len()).unwrap_or(u64::MAX);
            Ok(stat)
        } else if qid == Qid::file(8) {
            let mut stat = Stat::new("stream", qid, 0o444);
            stat.length = u64::try_from(NAMESPACE_CHANGE.len()).unwrap_or(u64::MAX);
            Ok(stat)
        } else {
            Ok(Stat::new(".", qid, DMDIR | 0o555))
        }
    }
}

fn data_stat() -> Stat {
    let mut stat = Stat::new("data", Qid::file(2), 0o444);
    stat.length = 6;
    stat
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

fn run_session_until_stdout_contains(args: &[&str], needle: &str) -> TestResult<Output> {
    let mut last = None;
    for _ in 0..50 {
        let output = run_r9p(args)?;
        if output.status.success() && String::from_utf8_lossy(&output.stdout).contains(needle) {
            return Ok(output);
        }
        last = Some(output);
        thread::sleep(Duration::from_millis(20));
    }
    let output = last.ok_or_else(|| test_error("session command did not run"))?;
    Err(test_error(format!(
        "session command never printed {needle} stdout={} stderr={}",
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
        .or_else(r9p_bin_next_to_current_test)
        .unwrap_or_else(|| PathBuf::from("target/debug/r9p"))
}

fn r9p_bin_next_to_current_test() -> Option<PathBuf> {
    let mut path = std::env::current_exe().ok()?;
    path.pop();
    if path.file_name().is_some_and(|name| name == "deps") {
        path.pop();
    }
    path.push(format!("r9p{}", std::env::consts::EXE_SUFFIX));
    path.exists().then_some(path)
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
