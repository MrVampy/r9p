use crate::{
    blocking::{Client, OREAD, OWRITE},
    export_descriptor::ExportDescriptor,
    qid::DMDIR,
    Error, Result,
};
use std::{
    net::{Shutdown, TcpStream},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

pub const DEFAULT_MAINTAIN_RETRY_INTERVAL: Duration = Duration::from_millis(250);
pub const DEFAULT_MAINTAIN_RENEW_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct R9pExportPublication {
    pub vault_endpoint_bind: String,
    pub vault_uname: String,
    pub vault_aname: String,
    pub service_name: String,
    pub descriptor: ExportDescriptor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishOutcome {
    AlreadyReady,
    Registered,
    Updated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct R9pExportMaintenanceConfig {
    pub retry_interval: Duration,
}

impl Default for R9pExportMaintenanceConfig {
    fn default() -> Self {
        Self {
            retry_interval: DEFAULT_MAINTAIN_RETRY_INTERVAL,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaintenanceSnapshot {
    pub success_count: u64,
    pub failure_count: u64,
    pub last_success: Option<PublishOutcome>,
    pub last_error: Option<String>,
}

#[derive(Debug)]
struct MaintenanceStatus {
    success_count: AtomicU64,
    failure_count: AtomicU64,
    last_success: Mutex<Option<PublishOutcome>>,
    last_error: Mutex<Option<String>>,
}

pub struct R9pExportMaintainer {
    signal: Arc<MaintenanceSignal>,
    join: Mutex<Option<JoinHandle<()>>>,
    status: Arc<MaintenanceStatus>,
}

#[derive(Debug)]
struct MaintenanceSignal {
    state: Mutex<MaintenanceSignalState>,
    condvar: Condvar,
}

#[derive(Debug)]
struct MaintenanceSignalState {
    stop: bool,
    pending: bool,
    active_wait: Option<TcpStream>,
}

pub fn publish_r9p_export(publication: &R9pExportPublication) -> Result<PublishOutcome> {
    publish_r9p_export_with_ready_action(publication, ReadyAction::Renew)
}

fn publish_r9p_export_with_ready_action(
    publication: &R9pExportPublication,
    ready_action: ReadyAction,
) -> Result<PublishOutcome> {
    validate_service_name(&publication.service_name)?;
    let descriptor = publication.descriptor.render()?;
    let _validated = ExportDescriptor::parse(&descriptor)?;
    let mut client = Client::connect_tcp(
        &publication.vault_endpoint_bind,
        &publication.vault_uname,
        &publication.vault_aname,
        publication.descriptor.msize,
    )?;
    publish_with_client(publication, &descriptor, &mut client, ready_action)
}

pub fn maintain_r9p_export(
    publication: R9pExportPublication,
    config: R9pExportMaintenanceConfig,
) -> Result<R9pExportMaintainer> {
    if config.retry_interval.is_zero() {
        return Err(Error::from(
            "r9p export maintenance retry interval must be non-zero",
        ));
    }
    let first_outcome = publish_r9p_export(&publication)?;
    let status = Arc::new(MaintenanceStatus::new(first_outcome));
    let signal = Arc::new(MaintenanceSignal {
        state: Mutex::new(MaintenanceSignalState {
            stop: false,
            pending: false,
            active_wait: None,
        }),
        condvar: Condvar::new(),
    });
    let thread_status = Arc::clone(&status);
    let thread_signal = Arc::clone(&signal);
    let join = thread::Builder::new()
        .name(format!(
            "r9p-srv-publish-{}",
            publication.service_name.replace('/', "_")
        ))
        .spawn(move || maintain_loop(publication, config, thread_signal, thread_status))
        .map_err(|error| Error::from(format!("spawn r9p export maintainer: {error}")))?;
    Ok(R9pExportMaintainer {
        signal,
        join: Mutex::new(Some(join)),
        status,
    })
}

impl R9pExportMaintainer {
    pub fn reconcile_now(&self) {
        if let Ok(mut state) = self.signal.state.lock() {
            state.pending = true;
            interrupt_active_wait(&state);
            self.signal.condvar.notify_all();
        }
    }

    pub fn status(&self) -> MaintenanceSnapshot {
        self.status.snapshot()
    }

    pub fn shutdown(&self) {
        if let Ok(mut state) = self.signal.state.lock() {
            state.stop = true;
            state.pending = true;
            interrupt_active_wait(&state);
            self.signal.condvar.notify_all();
        }
        if let Ok(mut join) = self.join.lock() {
            if let Some(join) = join.take() {
                let _ = join.join();
            }
        }
    }
}

impl Drop for R9pExportMaintainer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl MaintenanceStatus {
    fn new(first_outcome: PublishOutcome) -> Self {
        Self {
            success_count: AtomicU64::new(1),
            failure_count: AtomicU64::new(0),
            last_success: Mutex::new(Some(first_outcome)),
            last_error: Mutex::new(None),
        }
    }

    fn record_success(&self, outcome: PublishOutcome) {
        self.success_count.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut last_success) = self.last_success.lock() {
            *last_success = Some(outcome);
        }
        if let Ok(mut last_error) = self.last_error.lock() {
            *last_error = None;
        }
    }

    fn record_failure(&self, error: &Error) {
        self.failure_count.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut last_error) = self.last_error.lock() {
            *last_error = Some(error.display_lossy().to_string());
        }
    }

    fn snapshot(&self) -> MaintenanceSnapshot {
        MaintenanceSnapshot {
            success_count: self.success_count.load(Ordering::Relaxed),
            failure_count: self.failure_count.load(Ordering::Relaxed),
            last_success: self
                .last_success
                .lock()
                .ok()
                .and_then(|last_success| *last_success),
            last_error: self
                .last_error
                .lock()
                .ok()
                .and_then(|last_error| last_error.clone()),
        }
    }
}

fn maintain_loop(
    publication: R9pExportPublication,
    config: R9pExportMaintenanceConfig,
    signal: Arc<MaintenanceSignal>,
    status: Arc<MaintenanceStatus>,
) {
    let mut action = MaintenanceAction::Wait;
    loop {
        if stop_requested(&signal) {
            break;
        }
        match action {
            MaintenanceAction::Wait => match wait_for_srv_change(&publication, &signal) {
                MaintenanceWait::Changed | MaintenanceWait::Interrupted => {
                    action = MaintenanceAction::Reconcile;
                }
                MaintenanceWait::Renew => {
                    action = MaintenanceAction::Renew;
                }
                MaintenanceWait::Failed(error) => {
                    status.record_failure(&error);
                    if !wait_for_retry(&signal, config.retry_interval) {
                        break;
                    }
                    action = MaintenanceAction::Renew;
                }
            },
            MaintenanceAction::Reconcile | MaintenanceAction::Renew => {
                let ready_action = if action == MaintenanceAction::Renew {
                    ReadyAction::Renew
                } else {
                    ReadyAction::Observe
                };
                match publish_r9p_export_with_ready_action(&publication, ready_action) {
                    Ok(outcome) => {
                        status.record_success(outcome);
                        action = MaintenanceAction::Wait;
                    }
                    Err(error) => {
                        status.record_failure(&error);
                        if !wait_for_retry(&signal, config.retry_interval) {
                            break;
                        }
                        action = MaintenanceAction::Renew;
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MaintenanceAction {
    Wait,
    Reconcile,
    Renew,
}

#[derive(Debug)]
enum MaintenanceWait {
    Changed,
    Interrupted,
    Renew,
    Failed(Error),
}

fn wait_for_retry(signal: &MaintenanceSignal, interval: Duration) -> bool {
    let mut state = match signal.state.lock() {
        Ok(state) => state,
        Err(_) => return false,
    };
    if state.stop {
        return false;
    }
    if state.pending {
        state.pending = false;
        return true;
    }
    let result = signal
        .condvar
        .wait_timeout_while(state, interval, |state| !state.stop && !state.pending);
    state = match result {
        Ok((state, _)) => state,
        Err(_) => return false,
    };
    if state.pending {
        state.pending = false;
    }
    !state.stop
}

fn wait_for_srv_change(
    publication: &R9pExportPublication,
    signal: &MaintenanceSignal,
) -> MaintenanceWait {
    wait_for_srv_change_for(publication, signal, DEFAULT_MAINTAIN_RENEW_INTERVAL)
}

fn wait_for_srv_change_for(
    publication: &R9pExportPublication,
    signal: &MaintenanceSignal,
    renew_interval: Duration,
) -> MaintenanceWait {
    let (mut client, interrupt_stream) =
        match connect_interruptible_client(publication, Some(renew_interval)) {
            Ok(client) => client,
            Err(error) => return MaintenanceWait::Failed(error),
        };
    if !activate_wait(signal, interrupt_stream) {
        return MaintenanceWait::Interrupted;
    }
    let result = wait_for_srv_change_with_client(publication, &mut client);
    clear_active_wait(signal);
    if stop_requested(signal) || consume_pending(signal) {
        return MaintenanceWait::Interrupted;
    }
    match result {
        Ok(()) => MaintenanceWait::Changed,
        Err(error) if looks_timeout(&error) => MaintenanceWait::Renew,
        Err(error) => MaintenanceWait::Failed(error),
    }
}

fn wait_for_srv_change_with_client<S: std::io::Read + std::io::Write>(
    publication: &R9pExportPublication,
    client: &mut Client<S>,
) -> Result<()> {
    let state_path = srv_wait_state_path(&publication.service_name);
    let state = read_file(client, &state_path)?;
    let Some(token) = field_value(&state, "state_token") else {
        return Err(Error::from(format!(
            "srv wait state missing state_token: {state_path}"
        )));
    };
    let Some(state_name) = field_value(&state, "state") else {
        return Err(Error::from(format!(
            "srv wait state missing state: {state_path}"
        )));
    };
    if state_name != "ready" {
        return Ok(());
    }
    let wait_path = srv_wait_changed_after_path(&publication.service_name, &token);
    let _ = read_file(client, &wait_path)?;
    Ok(())
}

fn connect_interruptible_client(
    publication: &R9pExportPublication,
    read_timeout: Option<Duration>,
) -> Result<(Client<TcpStream>, TcpStream)> {
    let stream = TcpStream::connect(&publication.vault_endpoint_bind).map_err(|error| {
        Error::from(format!(
            "connect {}: {error}",
            publication.vault_endpoint_bind
        ))
    })?;
    stream
        .set_nodelay(true)
        .map_err(|error| Error::from(format!("set TCP_NODELAY: {error}")))?;
    stream
        .set_read_timeout(read_timeout)
        .map_err(|error| Error::from(format!("set read timeout: {error}")))?;
    let interrupt_stream = stream
        .try_clone()
        .map_err(|error| Error::from(format!("clone wait stream: {error}")))?;
    let client = Client::connect(
        stream,
        &publication.vault_uname,
        &publication.vault_aname,
        publication.descriptor.msize,
    )?;
    Ok((client, interrupt_stream))
}

fn activate_wait(signal: &MaintenanceSignal, stream: TcpStream) -> bool {
    let mut state = match signal.state.lock() {
        Ok(state) => state,
        Err(_) => {
            let _ = stream.shutdown(Shutdown::Both);
            return false;
        }
    };
    if state.stop || state.pending {
        let _ = stream.shutdown(Shutdown::Both);
        return false;
    }
    state.active_wait = Some(stream);
    true
}

fn clear_active_wait(signal: &MaintenanceSignal) {
    if let Ok(mut state) = signal.state.lock() {
        state.active_wait = None;
    }
}

fn consume_pending(signal: &MaintenanceSignal) -> bool {
    let mut state = match signal.state.lock() {
        Ok(state) => state,
        Err(_) => return false,
    };
    if state.pending {
        state.pending = false;
        true
    } else {
        false
    }
}

fn stop_requested(signal: &MaintenanceSignal) -> bool {
    signal.state.lock().map(|state| state.stop).unwrap_or(true)
}

fn interrupt_active_wait(state: &MaintenanceSignalState) {
    if let Some(stream) = &state.active_wait {
        let _ = stream.shutdown(Shutdown::Both);
    }
}

fn publish_with_client<S: std::io::Read + std::io::Write>(
    publication: &R9pExportPublication,
    descriptor: &str,
    client: &mut Client<S>,
    ready_action: ReadyAction,
) -> Result<PublishOutcome> {
    let srv_path = srv_path(&publication.service_name);
    match inspect_srv_path(client, &srv_path) {
        Ok(SrvPathState::File(summary)) if ready_summary_matches(&summary, publication)? => {
            if ready_action == ReadyAction::Renew {
                client.write_file(&srv_path, descriptor.as_bytes())?;
            }
            Ok(PublishOutcome::AlreadyReady)
        }
        Ok(SrvPathState::File(_)) => {
            client.write_file(&srv_path, descriptor.as_bytes())?;
            Ok(PublishOutcome::Updated)
        }
        Ok(SrvPathState::Missing) => {
            create_and_write(client, &publication.service_name, descriptor)?;
            Ok(PublishOutcome::Registered)
        }
        Err(error) if looks_missing(&error) => {
            create_and_write(client, &publication.service_name, descriptor)?;
            Ok(PublishOutcome::Registered)
        }
        Err(error) => Err(Error::from(format!("inspect {srv_path}: {error}"))),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadyAction {
    Observe,
    Renew,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SrvPathState {
    File(String),
    Missing,
}

fn inspect_srv_path<S: std::io::Read + std::io::Write>(
    client: &mut Client<S>,
    path: &str,
) -> Result<SrvPathState> {
    match client.stat_path(path) {
        Ok(stat) if stat.mode & DMDIR != 0 => Ok(SrvPathState::Missing),
        Ok(_) => read_file(client, path).map(SrvPathState::File),
        Err(error) if looks_missing(&error) => Ok(SrvPathState::Missing),
        Err(error) => Err(error),
    }
}

pub fn ready_summary_matches(summary: &str, publication: &R9pExportPublication) -> Result<bool> {
    let descriptor = &publication.descriptor;
    let endpoint = format!(
        "endpoint: inline:r9p-export:{}:{}:{}:{}\n",
        publication.service_name,
        descriptor.endpoint_bind,
        descriptor.uname,
        descriptor.vault_transport_class()?
    );
    Ok(
        summary.contains(&format!("service: {}\n", publication.service_name))
            && summary.contains("channel_kind: peer_namespace\n")
            && summary.contains(&format!(
                "channel: r9p-export:{}\n",
                publication.service_name
            ))
            && summary.contains(&endpoint)
            && summary.contains(&format!("aname: {}\n", descriptor.aname))
            && summary.contains(&format!("exported_root: {}\n", descriptor.exported_root)),
    )
}

fn create_and_write<S: std::io::Read + std::io::Write>(
    client: &mut Client<S>,
    service_name: &str,
    descriptor: &str,
) -> Result<()> {
    let (parent_path, create_name) = srv_create_parent_and_name(service_name)?;
    let parent = client.walk_path(parent_path)?;
    let (fid, _) = client.create(parent, create_name.as_bytes(), 0o666, OWRITE)?;
    let write_result = client.write(fid, 0, descriptor.as_bytes());
    let clunk_result = client.clunk(fid);
    write_result?;
    clunk_result?;
    Ok(())
}

fn read_file<S: std::io::Read + std::io::Write>(
    client: &mut Client<S>,
    path: &str,
) -> Result<String> {
    let fid = client.walk_path(path)?;
    client.open(fid, OREAD)?;
    let bytes = client.read(fid, 0, 8192);
    let clunk_result = client.clunk(fid);
    let bytes = bytes?;
    clunk_result?;
    String::from_utf8(bytes)
        .map_err(|error| Error::from(format!("read {path} was not utf-8: {error}")))
}

fn srv_path(service_name: &str) -> String {
    format!("/srv/{service_name}")
}

fn srv_wait_state_path(service_name: &str) -> String {
    format!("/srv/wait/{service_name}/state")
}

fn srv_wait_changed_after_path(service_name: &str, token: &str) -> String {
    format!("/srv/wait/{service_name}/changed-after/{token}")
}

fn field_value(report: &str, field: &str) -> Option<String> {
    let prefix = format!("{field}: ");
    report
        .lines()
        .find_map(|line| line.strip_prefix(&prefix).map(str::to_string))
}

fn validate_service_name(service_name: &str) -> Result<()> {
    if service_name.is_empty() || service_name.trim() != service_name {
        return Err(Error::from(format!(
            "invalid srv service name {service_name}"
        )));
    }
    let segments: Vec<&str> = service_name.split('/').collect();
    if segments.first().copied() == Some("wait")
        || segments.iter().any(|segment| {
            segment.is_empty()
                || segment.trim() != *segment
                || *segment == "."
                || *segment == ".."
                || segment.contains('\n')
                || segment.contains('\r')
        })
    {
        return Err(Error::from(format!(
            "invalid srv service name {service_name}"
        )));
    }
    Ok(())
}

fn srv_create_parent_and_name(service_name: &str) -> Result<(&'static str, &str)> {
    validate_service_name(service_name)?;
    Ok(("/srv", service_name))
}

fn looks_missing(error: &Error) -> bool {
    let message = error.display_lossy().to_ascii_lowercase();
    message.contains("partial walk")
        || message.contains("not found")
        || message.contains("not_found")
        || message.contains("missing")
        || message.contains("does not exist")
}

fn looks_timeout(error: &Error) -> bool {
    let message = error.display_lossy().to_ascii_lowercase();
    message.contains("transport timeout") || message.contains("would-block")
}

#[cfg(test)]
#[path = "srv_publish/tests.rs"]
mod tests;
