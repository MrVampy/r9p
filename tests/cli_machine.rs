use std::{
    error::Error,
    fs,
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{SystemTime, UNIX_EPOCH},
};

use r9p::{
    blocking::OTRUNC,
    codec,
    fid::Fid,
    message::TMessage,
    qid::{Qid, DMDIR},
    server::{FileTree, OpenFile, ReadData, Server},
    stat::Stat,
    Error as R9pError, Result as R9pResult,
};

type TestResult<T> = Result<T, Box<dyn Error>>;

#[derive(Clone)]
struct SharedFile {
    data: Arc<Mutex<Vec<u8>>>,
    read_calls: Arc<AtomicUsize>,
}

impl SharedFile {
    fn new(data: Vec<u8>) -> Self {
        Self {
            data: Arc::new(Mutex::new(data)),
            read_calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn bytes(&self) -> TestResult<Vec<u8>> {
        let data = self
            .data
            .lock()
            .map_err(|_| test_error("shared file lock poisoned"))?;
        Ok(data.clone())
    }

    fn read_count(&self) -> usize {
        self.read_calls.load(Ordering::SeqCst)
    }
}

struct MachineTree {
    root: Qid,
    data_qid: Qid,
    file: SharedFile,
}

impl MachineTree {
    fn new(file: SharedFile) -> Self {
        Self {
            root: Qid::dir(1),
            data_qid: Qid::file(2),
            file,
        }
    }
}

impl FileTree for MachineTree {
    fn attach(&mut self, _fid: Fid, _uname: &[u8], _aname: &[u8]) -> R9pResult<Qid> {
        Ok(self.root)
    }

    fn walk(
        &mut self,
        _fid: Fid,
        _newfid: Fid,
        start: Qid,
        names: &[Vec<u8>],
    ) -> R9pResult<Vec<Qid>> {
        if start == self.root && names == [b"data".to_vec()] {
            Ok(vec![self.data_qid])
        } else {
            Ok(Vec::new())
        }
    }

    fn open(&mut self, _fid: Fid, qid: Qid, mode: u8) -> R9pResult<OpenFile> {
        if qid == self.data_qid && mode & OTRUNC != 0 {
            let mut data = self
                .file
                .data
                .lock()
                .map_err(|_| R9pError::from("shared file lock poisoned"))?;
            data.clear();
        }
        Ok(OpenFile { qid, iounit: 0 })
    }

    fn read(&mut self, _fid: Fid, qid: Qid, offset: u64, count: u32) -> R9pResult<ReadData> {
        if qid != self.data_qid {
            return Ok(ReadData::Directory(Vec::new()));
        }
        self.file.read_calls.fetch_add(1, Ordering::SeqCst);
        let data = self
            .file
            .data
            .lock()
            .map_err(|_| R9pError::from("shared file lock poisoned"))?;
        let start = usize::try_from(offset)
            .map_err(|_| R9pError::from("read offset too large"))?
            .min(data.len());
        let end = start
            .saturating_add(usize::try_from(count).unwrap_or(usize::MAX))
            .min(data.len());
        Ok(ReadData::Bytes(data[start..end].to_vec()))
    }

    fn write(&mut self, _fid: Fid, qid: Qid, offset: u64, data: &[u8]) -> R9pResult<u32> {
        if qid != self.data_qid {
            return Err(R9pError::from("not writable"));
        }
        let start = usize::try_from(offset).map_err(|_| R9pError::from("offset too large"))?;
        let end = start
            .checked_add(data.len())
            .ok_or_else(|| R9pError::from("write overflow"))?;
        let mut current = self
            .file
            .data
            .lock()
            .map_err(|_| R9pError::from("shared file lock poisoned"))?;
        if current.len() < start {
            current.resize(start, 0);
        }
        if current.len() < end {
            current.resize(end, 0);
        }
        current[start..end].copy_from_slice(data);
        u32::try_from(data.len()).map_err(|_| R9pError::from("write too large"))
    }

    fn stat(&mut self, qid: Qid) -> R9pResult<Stat> {
        if qid == self.data_qid {
            let mut stat = Stat::new("data", qid, 0o600);
            stat.length = u64::try_from(
                self.file
                    .data
                    .lock()
                    .map_err(|_| R9pError::from("shared file lock poisoned"))?
                    .len(),
            )
            .map_err(|_| R9pError::from("length too large"))?;
            Ok(stat)
        } else {
            Ok(Stat::new(".", qid, DMDIR | 0o500))
        }
    }
}

#[test]
fn machine_readfd_streams_raw_stdout() -> TestResult<()> {
    let payload = large_payload();
    let shared = SharedFile::new(payload.clone());
    let (address, handle) = start_server(shared.clone())?;

    let output = run_machine(&address, &["readfd", "/data"], None)?;
    assert_success(&output)?;
    if output.stdout != payload {
        return Err(test_error("readfd stdout did not match payload"));
    }
    if shared.read_count() < 2 {
        return Err(test_error("readfd did not exercise chunked reads"));
    }
    join_server(handle)
}

#[test]
fn machine_read_to_streams_to_local_file_and_reports_count() -> TestResult<()> {
    let payload = large_payload();
    let shared = SharedFile::new(payload.clone());
    let output_path = temp_path("read-to");
    let output_arg = output_path.to_string_lossy().into_owned();
    let (address, handle) = start_server(shared)?;

    let output = run_machine(&address, &["read-to", "/data", &output_arg], None)?;
    assert_success(&output)?;
    assert_stdout(&output, &format!("read\t{}\n", payload.len()))?;
    let written = fs::read(&output_path)?;
    let _ = fs::remove_file(&output_path);
    if written != payload {
        return Err(test_error("read-to file did not match payload"));
    }
    join_server(handle)
}

#[test]
fn machine_write_at_streams_stdin_and_reports_count() -> TestResult<()> {
    let payload = large_payload();
    let shared = SharedFile::new(b"0123456789".to_vec());
    let (address, handle) = start_server(shared.clone())?;

    let output = run_machine(&address, &["write-at", "/data", "4"], Some(&payload))?;
    assert_success(&output)?;
    assert_stdout(&output, &format!("write\t{}\n", payload.len()))?;

    let mut expected = b"0123".to_vec();
    expected.extend(payload);
    if shared.bytes()? != expected {
        return Err(test_error("write-at did not update server file at offset"));
    }
    join_server(handle)
}

#[test]
fn machine_write_from_streams_local_file_and_reports_count() -> TestResult<()> {
    let payload = large_payload();
    let input_path = temp_path("write-from");
    fs::write(&input_path, &payload)?;
    let input_arg = input_path.to_string_lossy().into_owned();
    let shared = SharedFile::new(b"abcdef".to_vec());
    let (address, handle) = start_server(shared.clone())?;

    let output = run_machine(&address, &["write-from", "/data", "2", &input_arg], None)?;
    let _ = fs::remove_file(&input_path);
    assert_success(&output)?;
    assert_stdout(&output, &format!("write\t{}\n", payload.len()))?;

    let mut expected = b"ab".to_vec();
    expected.extend(payload);
    if shared.bytes()? != expected {
        return Err(test_error(
            "write-from did not update server file at offset",
        ));
    }
    join_server(handle)
}

#[test]
fn machine_writefd_streams_stdin_with_truncation() -> TestResult<()> {
    let payload = b"replacement\n".to_vec();
    let shared = SharedFile::new(b"old content\n".to_vec());
    let (address, handle) = start_server(shared.clone())?;

    let output = run_machine(&address, &["writefd", "/data"], Some(&payload))?;
    assert_success(&output)?;
    assert_stdout(&output, &format!("write\t{}\n", payload.len()))?;
    if shared.bytes()? != payload {
        return Err(test_error("writefd did not truncate before writing"));
    }
    join_server(handle)
}

fn start_server(file: SharedFile) -> TestResult<(String, JoinHandle<Result<(), String>>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?.to_string();
    let handle = thread::spawn(move || -> Result<(), String> {
        let (mut stream, _) = listener
            .accept()
            .map_err(|error| format!("accept: {error}"))?;
        let mut server = Server::new(MachineTree::new(file));
        while let Some(message) = read_tmessage(&mut stream)? {
            let reply = server.handle(message);
            let frame = codec::encode_rmessage_checked(&reply, server.session().msize())
                .map_err(|error| format!("encode reply: {error}"))?;
            stream
                .write_all(&frame)
                .map_err(|error| format!("write reply: {error}"))?;
        }
        Ok(())
    });
    Ok((address, handle))
}

fn read_tmessage(stream: &mut TcpStream) -> Result<Option<TMessage>, String> {
    let mut prefix = [0_u8; 4];
    match stream.read_exact(&mut prefix) {
        Ok(()) => {}
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(format!("read frame size: {error}")),
    }
    let size = u32::from_le_bytes(prefix);
    if size < codec::FRAME_HEADER_SIZE {
        return Err(format!("short frame: {size}"));
    }
    let rest_len = usize::try_from(size - 4).map_err(|_| "frame too large".to_string())?;
    let mut frame = Vec::with_capacity(rest_len + 4);
    frame.extend(prefix);
    frame.resize(rest_len + 4, 0);
    stream
        .read_exact(&mut frame[4..])
        .map_err(|error| format!("read frame body: {error}"))?;
    codec::decode_tmessage(&frame)
        .map(Some)
        .map_err(|error| format!("decode request: {error}"))
}

fn run_machine(address: &str, args: &[&str], stdin: Option<&[u8]>) -> TestResult<Output> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_r9p"));
    command.args([
        "--machine",
        "-a",
        address,
        "-u",
        "test",
        "-A",
        "/",
        "-m",
        "8192",
    ]);
    command.args(args);
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    if stdin.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command.spawn()?;
    if let Some(input) = stdin {
        let mut child_stdin = child
            .stdin
            .take()
            .ok_or_else(|| test_error("child stdin unavailable"))?;
        child_stdin.write_all(input)?;
    }
    Ok(child.wait_with_output()?)
}

fn assert_success(output: &Output) -> TestResult<()> {
    if output.status.success() {
        Ok(())
    } else {
        Err(test_error(format!(
            "command failed status={:?} stdout={} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

fn assert_stdout(output: &Output, expected: &str) -> TestResult<()> {
    if output.stdout == expected.as_bytes() {
        Ok(())
    } else {
        Err(test_error(format!(
            "unexpected stdout: expected {:?}, got {:?}",
            expected,
            String::from_utf8_lossy(&output.stdout)
        )))
    }
}

fn join_server(handle: JoinHandle<Result<(), String>>) -> TestResult<()> {
    match handle.join() {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(test_error(error)),
        Err(_) => Err(test_error("server thread panicked")),
    }
}

fn large_payload() -> Vec<u8> {
    (0..200_000)
        .map(|index| b'a' + u8::try_from(index % 26).unwrap_or(0))
        .collect()
}

fn temp_path(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    std::env::temp_dir().join(format!(
        "r9p-cli-machine-{}-{nanos}-{label}",
        std::process::id()
    ))
}

fn test_error(message: impl Into<String>) -> Box<dyn Error> {
    Box::new(io::Error::other(message.into()))
}
