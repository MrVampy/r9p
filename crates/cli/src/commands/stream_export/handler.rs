use std::{
    collections::{BTreeSet, VecDeque},
    io::{Read, Write},
    os::unix::process::CommandExt,
    process::{Child, ChildStdout, Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex, MutexGuard,
    },
    thread::{self, JoinHandle},
};

use r9p::{
    error::EPERM,
    fid::Fid,
    mode::{self, OREAD, OWRITE},
    qid::{Qid, DMDIR},
    server::{
        ConnectionHandler, OpenFile, ReadData, ServerCompletion, ServerRequest, ServerRequestKind,
    },
    stat::Stat,
    Error, Result,
};
use rustix::{
    io::Errno,
    process::{kill_process_group, Pid, Signal},
};

use super::config::ProcessCommand;

const ROOT: Qid = Qid::dir(1);
const STREAM: Qid = Qid::file(2);
const OUTPUT_READ_BYTES: usize = 64 * 1024;

#[derive(Default)]
struct LifecycleState {
    opened: BTreeSet<Fid>,
    reader: Option<Fid>,
    writer: Option<Fid>,
    process_id: Option<u32>,
    started: bool,
}

#[derive(Default)]
struct InputState {
    stdin: Option<std::process::ChildStdin>,
    next_offset: u64,
}

#[derive(Default)]
struct OutputState {
    bytes: VecDeque<u8>,
    next_offset: u64,
    finished: bool,
    failure: Option<String>,
}

struct ProcessStreamInner {
    command: ProcessCommand,
    max_buffer_bytes: usize,
    lifecycle: Mutex<LifecycleState>,
    input: Mutex<InputState>,
    output: Mutex<OutputState>,
    output_changed: Condvar,
    process_thread: Mutex<Option<JoinHandle<()>>>,
    stopping: AtomicBool,
}

struct ProcessOwner {
    child: Option<Child>,
    stdout: ChildStdout,
    process_id: u32,
}

impl Drop for ProcessOwner {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = terminate_process_group(self.process_id);
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub(super) struct ProcessStream {
    inner: Arc<ProcessStreamInner>,
}

impl ProcessStream {
    pub(super) fn new(command: ProcessCommand, max_buffer_bytes: usize) -> Self {
        Self {
            inner: Arc::new(ProcessStreamInner {
                command,
                max_buffer_bytes,
                lifecycle: Mutex::new(LifecycleState::default()),
                input: Mutex::new(InputState::default()),
                output: Mutex::new(OutputState::default()),
                output_changed: Condvar::new(),
                process_thread: Mutex::new(None),
                stopping: AtomicBool::new(false),
            }),
        }
    }

    fn open(&self, fid: Fid, qid: Qid, open_mode: u8) -> Result<ServerCompletion> {
        if qid == ROOT {
            if !mode::is_directory_mode(open_mode) {
                return Err(Error::from_static("stream export root is read-only"));
            }
            return Ok(ServerCompletion::Open(OpenFile { qid, iounit: 0 }));
        }
        if qid != STREAM {
            return Err(Error::from_static("unknown stream export qid"));
        }

        let should_start = {
            let mut lifecycle = self.lock_lifecycle()?;
            if lifecycle.started || !lifecycle.opened.insert(fid) {
                return Err(Error::from_static("stream endpoint is already open"));
            }
            match open_mode {
                OREAD if lifecycle.reader.is_none() => lifecycle.reader = Some(fid),
                OWRITE if lifecycle.writer.is_none() => lifecycle.writer = Some(fid),
                _ => {
                    lifecycle.opened.remove(&fid);
                    return Err(Error::from_static(
                        "stream endpoint requires one read fid and one write fid",
                    ));
                }
            }
            lifecycle.reader.is_some() && lifecycle.writer.is_some()
        };

        if should_start {
            if let Err(error) = self.start_process() {
                self.clear_open_fids()?;
                return Err(error);
            }
        }
        Ok(ServerCompletion::Open(OpenFile { qid, iounit: 0 }))
    }

    fn start_process(&self) -> Result<()> {
        let mut command = Command::new(&self.inner.command.program);
        command
            .args(&self.inner.command.arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .process_group(0);
        let mut child = command.spawn().map_err(|error| {
            Error::new(format!(
                "start stream process {}: {error}",
                self.inner.command.program.display()
            ))
        })?;
        let process_id = child.id();
        let stdin = match child.stdin.take() {
            Some(stdin) => stdin,
            None => {
                terminate_child(&mut child);
                return Err(Error::from_static("stream process stdin unavailable"));
            }
        };
        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                terminate_child(&mut child);
                return Err(Error::from_static("stream process stdout unavailable"));
            }
        };
        let process = ProcessOwner {
            child: Some(child),
            stdout,
            process_id,
        };

        {
            let mut input = self.lock_input()?;
            input.stdin = Some(stdin);
            input.next_offset = 0;
        }
        {
            let mut output = self.lock_output()?;
            *output = OutputState::default();
        }
        {
            let mut lifecycle = self.lock_lifecycle()?;
            lifecycle.process_id = Some(process_id);
            lifecycle.started = true;
        }

        let inner = Arc::clone(&self.inner);
        let process_thread = thread::Builder::new()
            .name(format!("r9p-stream-process-{process_id}"))
            .spawn(move || run_process(inner, process));
        match process_thread {
            Ok(process_thread) => {
                *self.lock_process_thread()? = Some(process_thread);
                Ok(())
            }
            Err(error) => {
                self.inner.stopping.store(true, Ordering::SeqCst);
                self.clear_process_state()?;
                Err(Error::new(format!("start stream process worker: {error}")))
            }
        }
    }

    fn read(
        &self,
        fid: Fid,
        offset: u64,
        count: u32,
        cancel: Option<&AtomicBool>,
    ) -> Result<ServerCompletion> {
        if self.lock_lifecycle()?.reader != Some(fid) {
            return Err(Error::from_static("stream read used a non-reader fid"));
        }
        let count = usize::try_from(count)
            .map_err(|_| Error::from_static("stream read count too large"))?;
        let mut output = self.lock_output()?;
        loop {
            if offset != output.next_offset {
                return Err(Error::from_static("noncontiguous stream read"));
            }
            if !output.bytes.is_empty() {
                let count = count.min(output.bytes.len());
                let bytes = output.bytes.drain(..count).collect::<Vec<_>>();
                output.next_offset = output.next_offset.saturating_add(
                    u64::try_from(bytes.len())
                        .map_err(|_| Error::from_static("stream read offset overflow"))?,
                );
                self.inner.output_changed.notify_all();
                return Ok(ServerCompletion::Read(ReadData::Bytes(bytes)));
            }
            if let Some(failure) = &output.failure {
                return Err(Error::new(failure.clone()));
            }
            if output.finished {
                return Ok(ServerCompletion::Read(ReadData::Bytes(Vec::new())));
            }
            if self.inner.stopping.load(Ordering::SeqCst)
                || cancel.is_some_and(|flag| flag.load(Ordering::SeqCst))
            {
                return Err(Error::from_static("stream read cancelled"));
            }
            output = self
                .inner
                .output_changed
                .wait(output)
                .map_err(|_| Error::from_static("stream output state poisoned"))?;
        }
    }

    fn write(&self, fid: Fid, offset: u64, data: &[u8]) -> Result<ServerCompletion> {
        if self.lock_lifecycle()?.writer != Some(fid) {
            return Err(Error::from_static("stream write used a non-writer fid"));
        }
        if self.inner.stopping.load(Ordering::SeqCst) {
            return Err(Error::from_static("stream process is stopping"));
        }

        let write_result = {
            let mut input = self.lock_input()?;
            if offset != input.next_offset {
                return Err(Error::from_static("noncontiguous stream write"));
            }
            let stdin = input
                .stdin
                .as_mut()
                .ok_or_else(|| Error::from_static("stream process input is closed"))?;
            let write_result = stdin.write_all(data).and_then(|()| stdin.flush());
            if write_result.is_ok() {
                let count = u64::try_from(data.len())
                    .map_err(|_| Error::from_static("stream write offset overflow"))?;
                input.next_offset = input.next_offset.saturating_add(count);
            }
            write_result
        };
        if let Err(error) = write_result {
            let _ = self.request_stop();
            return Err(Error::new(format!("write stream process input: {error}")));
        }
        let count = u32::try_from(data.len())
            .map_err(|_| Error::from_static("stream write count too large"))?;
        Ok(ServerCompletion::Write { count })
    }

    fn clunk(&self, fid: Fid, qid: Qid) -> Result<ServerCompletion> {
        if qid != STREAM {
            return Ok(ServerCompletion::Clunk);
        }
        let (close_input, stop_process) = {
            let mut lifecycle = self.lock_lifecycle()?;
            lifecycle.opened.remove(&fid);
            let close_input = lifecycle.writer == Some(fid);
            let stop_process = lifecycle.reader == Some(fid);
            if close_input {
                lifecycle.writer = None;
            }
            if stop_process {
                lifecycle.reader = None;
            }
            (close_input, stop_process)
        };
        if close_input {
            self.lock_input()?.stdin.take();
        }
        if stop_process {
            self.request_stop()?;
        }
        Ok(ServerCompletion::Clunk)
    }

    fn request_stop(&self) -> Result<()> {
        self.inner.stopping.store(true, Ordering::SeqCst);
        self.lock_input()?.stdin.take();
        let process_id = self.lock_lifecycle()?.process_id;
        self.inner.output_changed.notify_all();
        if let Some(process_id) = process_id {
            terminate_process_group(process_id)?;
        }
        Ok(())
    }

    fn reset_session(&self) -> Result<()> {
        let stop_result = self.request_stop();
        let join_result = match self.lock_process_thread()?.take() {
            Some(process_thread) => process_thread
                .join()
                .map_err(|_| Error::from_static("stream process worker panicked")),
            None => Ok(()),
        };
        self.clear_process_state()?;
        stop_result?;
        join_result
    }

    fn clear_open_fids(&self) -> Result<()> {
        let mut lifecycle = self.lock_lifecycle()?;
        lifecycle.opened.clear();
        lifecycle.reader = None;
        lifecycle.writer = None;
        Ok(())
    }

    fn clear_process_state(&self) -> Result<()> {
        *self.lock_lifecycle()? = LifecycleState::default();
        *self.lock_input()? = InputState::default();
        *self.lock_output()? = OutputState::default();
        self.inner.stopping.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn lock_lifecycle(&self) -> Result<MutexGuard<'_, LifecycleState>> {
        self.inner
            .lifecycle
            .lock()
            .map_err(|_| Error::from_static("stream lifecycle state poisoned"))
    }

    fn lock_input(&self) -> Result<MutexGuard<'_, InputState>> {
        self.inner
            .input
            .lock()
            .map_err(|_| Error::from_static("stream input state poisoned"))
    }

    fn lock_output(&self) -> Result<MutexGuard<'_, OutputState>> {
        self.inner
            .output
            .lock()
            .map_err(|_| Error::from_static("stream output state poisoned"))
    }

    fn lock_process_thread(&self) -> Result<MutexGuard<'_, Option<JoinHandle<()>>>> {
        self.inner
            .process_thread
            .lock()
            .map_err(|_| Error::from_static("stream process worker state poisoned"))
    }
}

impl ConnectionHandler for ProcessStream {
    fn perform(
        &self,
        request: &ServerRequest,
        cancel: Option<&AtomicBool>,
    ) -> Result<ServerCompletion> {
        match &request.kind {
            ServerRequestKind::Attach { .. } => Ok(ServerCompletion::Attach { qid: ROOT }),
            ServerRequestKind::Walk { start, wnames, .. } => walk(*start, wnames),
            ServerRequestKind::Open { fid, qid, mode, .. } => self.open(*fid, *qid, *mode),
            ServerRequestKind::Read { qid, .. } if *qid == ROOT => {
                Ok(ServerCompletion::Read(ReadData::Directory(vec![
                    stream_stat(),
                ])))
            }
            ServerRequestKind::Read {
                fid,
                qid,
                offset,
                count,
            } if *qid == STREAM => self.read(*fid, *offset, *count, cancel),
            ServerRequestKind::Write {
                fid,
                qid,
                offset,
                data,
            } if *qid == STREAM => self.write(*fid, *offset, data),
            ServerRequestKind::Clunk { fid, qid } => self.clunk(*fid, *qid),
            ServerRequestKind::Stat { qid, .. } if *qid == ROOT => {
                Ok(ServerCompletion::Stat { stat: root_stat() })
            }
            ServerRequestKind::Stat { qid, .. } if *qid == STREAM => Ok(ServerCompletion::Stat {
                stat: stream_stat(),
            }),
            ServerRequestKind::Referrals { .. } => Ok(ServerCompletion::Referrals {
                referrals: Vec::new(),
            }),
            _ => Err(Error::from_static(EPERM)),
        }
    }

    fn is_async(&self, request: &ServerRequest) -> bool {
        matches!(
            request.kind,
            ServerRequestKind::Read { qid, .. } | ServerRequestKind::Write { qid, .. }
                if qid == STREAM
        )
    }

    fn cancellation_fid(&self, request: &ServerRequest) -> Option<Fid> {
        match request.kind {
            ServerRequestKind::Read { fid, qid, .. }
            | ServerRequestKind::Write { fid, qid, .. }
                if qid == STREAM =>
            {
                Some(fid)
            }
            _ => None,
        }
    }

    fn reset(&self) -> Result<()> {
        self.reset_session()
    }

    fn wake_after_cancel(&self) {
        self.inner.output_changed.notify_all();
    }
}

impl Drop for ProcessStream {
    fn drop(&mut self) {
        let _ = self.reset_session();
    }
}

fn walk(start: Qid, names: &[Vec<u8>]) -> Result<ServerCompletion> {
    let mut current = start;
    let mut qids = Vec::with_capacity(names.len());
    for name in names {
        current = match (current, name.as_slice()) {
            (qid, b".") => qid,
            (_, b"..") => ROOT,
            (qid, b"stream") if qid == ROOT => STREAM,
            _ => break,
        };
        qids.push(current);
    }
    Ok(ServerCompletion::Walk { qids })
}

fn root_stat() -> Stat {
    Stat::new(".", ROOT, DMDIR | 0o500)
}

fn stream_stat() -> Stat {
    Stat::new("stream", STREAM, 0o600)
}

fn run_process(inner: Arc<ProcessStreamInner>, mut process: ProcessOwner) {
    let mut failure = None;
    let mut buffer = [0_u8; OUTPUT_READ_BYTES];
    loop {
        match process.stdout.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                if let Err(error) = append_output(&inner, &buffer[..count]) {
                    failure = Some(error);
                    break;
                }
            }
            Err(error) => {
                failure = Some(format!("read stream process output: {error}"));
                break;
            }
        }
    }

    let status = process
        .child
        .take()
        .ok_or_else(|| std::io::Error::other("stream process owner lost child"))
        .and_then(|mut child| child.wait());
    if failure.is_none() {
        failure = match status {
            Ok(status) if status.success() => None,
            Ok(status) => Some(describe_exit(status)),
            Err(error) => Some(format!("wait for stream process: {error}")),
        };
    }
    if let Ok(mut output) = inner.output.lock() {
        output.failure = failure;
        output.finished = true;
        inner.output_changed.notify_all();
    }
    if let Ok(mut lifecycle) = inner.lifecycle.lock() {
        lifecycle.process_id = None;
    }
}

fn append_output(inner: &ProcessStreamInner, bytes: &[u8]) -> std::result::Result<(), String> {
    let mut position = 0_usize;
    while position < bytes.len() {
        let mut output = inner
            .output
            .lock()
            .map_err(|_| "stream output state poisoned".to_string())?;
        while output.bytes.len() >= inner.max_buffer_bytes && !inner.stopping.load(Ordering::SeqCst)
        {
            output = inner
                .output_changed
                .wait(output)
                .map_err(|_| "stream output state poisoned".to_string())?;
        }
        if inner.stopping.load(Ordering::SeqCst) {
            return Ok(());
        }
        let available = inner.max_buffer_bytes.saturating_sub(output.bytes.len());
        let end = position.saturating_add(available).min(bytes.len());
        output.bytes.extend(&bytes[position..end]);
        position = end;
        inner.output_changed.notify_all();
    }
    Ok(())
}

fn describe_exit(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("stream process exited with status {code}"),
        None => "stream process terminated without an exit status".to_string(),
    }
}

fn terminate_process_group(process_id: u32) -> Result<()> {
    let raw = i32::try_from(process_id)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or_else(|| Error::from_static("invalid stream process id"))?;
    match kill_process_group(raw, Signal::KILL) {
        Ok(()) | Err(Errno::SRCH) => Ok(()),
        Err(error) => Err(Error::new(format!(
            "terminate stream process group {process_id}: {error}"
        ))),
    }
}

fn terminate_child(child: &mut Child) {
    let _ = terminate_process_group(child.id());
    let _ = child.kill();
    let _ = child.wait();
}
