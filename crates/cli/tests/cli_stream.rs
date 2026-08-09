use std::{
    collections::BTreeSet,
    error::Error,
    io::Write,
    net::{TcpListener, TcpStream},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    thread::{self, JoinHandle},
};

use r9p::{
    fid::Fid,
    qid::Qid,
    server::{
        serve_connection, ConnectionHandler, OpenFile, ReadData, ServerCompletion, ServerConfig,
        ServerRequest, ServerRequestKind,
    },
    Error as R9pError, Result as R9pResult,
};

type TestResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

const ROOT: Qid = Qid::dir(1);
const STREAM: Qid = Qid::file(2);

#[derive(Default)]
struct StreamState {
    opened: BTreeSet<Fid>,
    writer: Option<Fid>,
    input: Vec<u8>,
    input_closed: bool,
}

#[derive(Default)]
struct EchoStream {
    state: Mutex<StreamState>,
    changed: Condvar,
}

impl ConnectionHandler for EchoStream {
    fn perform(
        &self,
        request: &ServerRequest,
        cancel: Option<&AtomicBool>,
    ) -> R9pResult<ServerCompletion> {
        match &request.kind {
            ServerRequestKind::Attach { .. } => Ok(ServerCompletion::Attach { qid: ROOT }),
            ServerRequestKind::Walk { start, wnames, .. }
                if *start == ROOT && wnames == &[b"stream".to_vec()] =>
            {
                Ok(ServerCompletion::Walk { qids: vec![STREAM] })
            }
            ServerRequestKind::Open { fid, qid, .. } if *qid == STREAM => {
                let mut state = self.lock_state()?;
                if !state.opened.insert(*fid) || state.opened.len() > 2 {
                    return Err(R9pError::from_static("unexpected stream open"));
                }
                if state.opened.len() == 2 {
                    state.writer = Some(*fid);
                }
                Ok(ServerCompletion::Open(OpenFile {
                    qid: STREAM,
                    iounit: 0,
                }))
            }
            ServerRequestKind::Read {
                qid, offset, count, ..
            } if *qid == STREAM => self.read(*offset, *count, cancel),
            ServerRequestKind::Write {
                fid,
                qid,
                offset,
                data,
            } if *qid == STREAM => {
                let mut state = self.lock_state()?;
                if state.writer != Some(*fid) {
                    return Err(R9pError::from_static("write used the reader fid"));
                }
                if usize::try_from(*offset).ok() != Some(state.input.len()) {
                    return Err(R9pError::from_static("noncontiguous stream write"));
                }
                state.input.extend_from_slice(data);
                let count = u32::try_from(data.len())
                    .map_err(|_| R9pError::from_static("stream write too large"))?;
                self.changed.notify_all();
                Ok(ServerCompletion::Write { count })
            }
            ServerRequestKind::Clunk { fid, .. } => {
                let mut state = self.lock_state()?;
                if state.writer == Some(*fid) {
                    state.input_closed = true;
                    self.changed.notify_all();
                }
                Ok(ServerCompletion::Clunk)
            }
            ServerRequestKind::Referrals { .. } => Ok(ServerCompletion::Referrals {
                referrals: Vec::new(),
            }),
            _ => Err(R9pError::from_static("unsupported stream request")),
        }
    }

    fn is_async(&self, request: &ServerRequest) -> bool {
        matches!(request.kind, ServerRequestKind::Read { .. })
    }

    fn cancellation_fid(&self, request: &ServerRequest) -> Option<Fid> {
        match request.kind {
            ServerRequestKind::Read { fid, .. } => Some(fid),
            _ => None,
        }
    }

    fn wake_after_cancel(&self) {
        self.changed.notify_all();
    }
}

impl EchoStream {
    fn lock_state(&self) -> R9pResult<std::sync::MutexGuard<'_, StreamState>> {
        self.state
            .lock()
            .map_err(|_| R9pError::from_static("stream state poisoned"))
    }

    fn read(
        &self,
        offset: u64,
        count: u32,
        cancel: Option<&AtomicBool>,
    ) -> R9pResult<ServerCompletion> {
        let start = usize::try_from(offset)
            .map_err(|_| R9pError::from_static("stream read offset too large"))?;
        let mut state = self.lock_state()?;
        loop {
            if start < state.input.len() {
                let end = start
                    .saturating_add(usize::try_from(count).unwrap_or(usize::MAX))
                    .min(state.input.len());
                return Ok(ServerCompletion::Read(ReadData::Bytes(
                    state.input[start..end].to_vec(),
                )));
            }
            if state.input_closed {
                return Ok(ServerCompletion::Read(ReadData::Bytes(Vec::new())));
            }
            if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
                return Err(R9pError::from_static("stream read cancelled"));
            }
            state = self
                .changed
                .wait(state)
                .map_err(|_| R9pError::from_static("stream state poisoned"))?;
        }
    }
}

#[test]
fn stream_is_full_duplex_and_byte_transparent() -> TestResult<()> {
    let (address, server) = start_server()?;
    let mut input = vec![0x12, 0x00, b'\r', 0xff, b'\n'];
    input.extend((0_u32..12_000).map(|value| value.wrapping_mul(31) as u8));

    let mut child = Command::new(env!("CARGO_BIN_EXE_r9p"))
        .args([
            "-a", &address, "-u", "test", "-A", "/", "-m", "8192", "stream", "/stream",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("stream stdin unavailable")?
        .write_all(&input)?;

    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(format!(
            "stream failed status={:?} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    if output.stdout != input {
        return Err("stream changed relayed bytes".into());
    }
    join_server(server)
}

fn start_server() -> TestResult<(String, JoinHandle<R9pResult<()>>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?.to_string();
    let server = thread::spawn(move || {
        let (stream, _) = listener
            .accept()
            .map_err(|error| R9pError::new(format!("accept stream client: {error}")))?;
        serve_stream(stream)
    });
    Ok((address, server))
}

fn serve_stream(stream: TcpStream) -> R9pResult<()> {
    serve_connection(
        stream,
        ServerConfig {
            max_async_requests: 4,
            ..ServerConfig::default()
        },
        Arc::new(EchoStream::default()),
    )
}

fn join_server(server: JoinHandle<R9pResult<()>>) -> TestResult<()> {
    server.join().map_err(|_| "stream server panicked")??;
    Ok(())
}
