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
    validate_service_name(&publication.service_name)?;
    let descriptor = publication.descriptor.render()?;
    let _validated = ExportDescriptor::parse(&descriptor)?;
    let mut client = Client::connect_tcp(
        &publication.vault_endpoint_bind,
        &publication.vault_uname,
        &publication.vault_aname,
        publication.descriptor.msize,
    )?;
    publish_with_client(publication, &descriptor, &mut client)
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
    loop {
        if stop_requested(&signal) {
            break;
        }
        match publish_r9p_export(&publication) {
            Ok(outcome) => {
                status.record_success(outcome);
                match wait_for_srv_change(&publication, &signal) {
                    MaintenanceWait::Changed | MaintenanceWait::Interrupted => {}
                    MaintenanceWait::Failed(error) => {
                        status.record_failure(&error);
                        if !wait_for_retry(&signal, config.retry_interval) {
                            break;
                        }
                    }
                }
            }
            Err(error) => {
                status.record_failure(&error);
                if !wait_for_retry(&signal, config.retry_interval) {
                    break;
                }
            }
        }
    }
}

#[derive(Debug)]
enum MaintenanceWait {
    Changed,
    Interrupted,
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
    let (mut client, interrupt_stream) = match connect_interruptible_client(publication) {
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
) -> Result<PublishOutcome> {
    let srv_path = srv_path(&publication.service_name);
    match inspect_srv_path(client, &srv_path) {
        Ok(SrvPathState::File(summary)) if ready_summary_matches(&summary, publication)? => {
            Ok(PublishOutcome::AlreadyReady)
        }
        Ok(SrvPathState::File(_)) => {
            write_existing(client, &srv_path, descriptor)?;
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
    let (parent_path, leaf_name) = srv_parent_and_leaf(service_name)?;
    let parent = client.walk_path(&parent_path)?;
    let (fid, _) = client.create(parent, leaf_name.as_bytes(), 0o666, OWRITE)?;
    let write_result = client.write(fid, 0, descriptor.as_bytes());
    let clunk_result = client.clunk(fid);
    write_result?;
    clunk_result?;
    Ok(())
}

fn write_existing<S: std::io::Read + std::io::Write>(
    client: &mut Client<S>,
    path: &str,
    descriptor: &str,
) -> Result<()> {
    let fid = client.walk_path(path)?;
    client.open(fid, OWRITE)?;
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

fn srv_parent_and_leaf(service_name: &str) -> Result<(String, &str)> {
    validate_service_name(service_name)?;
    let mut segments = service_name.split('/').collect::<Vec<_>>();
    let leaf = segments
        .pop()
        .ok_or_else(|| Error::from(format!("invalid srv service name {service_name}")))?;
    let parent = if segments.is_empty() {
        "/srv".to_string()
    } else {
        format!("/srv/{}", segments.join("/"))
    };
    Ok((parent, leaf))
}

fn looks_missing(error: &Error) -> bool {
    let message = error.display_lossy().to_ascii_lowercase();
    message.contains("partial walk")
        || message.contains("not found")
        || message.contains("not_found")
        || message.contains("missing")
        || message.contains("does not exist")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        codec,
        export_descriptor::{AuthBoundary, ExportMode, Protocol, TransportClass},
        message::TMessage,
        qid::{Qid, DMDIR},
        server::{FileTree, OpenFile, ReadData, Server},
        stat::Stat,
    };
    use std::{
        collections::BTreeMap,
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        sync::{Arc, Mutex},
        thread,
    };

    #[test]
    fn publishes_missing_srv_entry() {
        let tree = SharedSrvTree::new();
        let address = serve_tree(tree.clone());
        let mut publication = publication(&address);
        publication.vault_endpoint_bind = address;

        let outcome = publish_r9p_export(&publication).expect("publish should succeed");

        assert_eq!(outcome, PublishOutcome::Registered);
        let descriptor = tree
            .content("polymarket")
            .expect("descriptor should be written");
        assert!(descriptor.contains("format\tr9p-export.v1\n"));
        assert!(descriptor.contains("endpoint_bind\t192.168.0.21:19590\n"));
    }

    #[test]
    fn publication_descriptor_can_carry_host_ownership() {
        let tree = SharedSrvTree::new();
        let address = serve_tree(tree.clone());
        let mut publication = publication(&address);
        publication.vault_endpoint_bind = address;
        publication.descriptor.extra_fields.insert(
            "service_unit".to_string(),
            "vault-polymarket-watcher.service".to_string(),
        );
        publication.descriptor.extra_fields.insert(
            "host_firewall_admission".to_string(),
            "tcp:192.168.0.21:19590".to_string(),
        );

        let outcome = publish_r9p_export(&publication).expect("publish should succeed");

        assert_eq!(outcome, PublishOutcome::Registered);
        let descriptor = tree
            .content("polymarket")
            .expect("descriptor should be written");
        assert!(descriptor.contains("service_unit\tvault-polymarket-watcher.service\n"));
        assert!(descriptor.contains("host_firewall_admission\ttcp:192.168.0.21:19590\n"));
    }

    #[test]
    fn publish_is_idempotent_when_ready_summary_matches() {
        let tree = SharedSrvTree::new();
        tree.set_ready_summary("polymarket", ready_summary("192.168.0.21:19590"));
        let address = serve_tree(tree.clone());
        let mut publication = publication(&address);
        publication.vault_endpoint_bind = address;

        let outcome = publish_r9p_export(&publication).expect("publish should succeed");

        assert_eq!(outcome, PublishOutcome::AlreadyReady);
        assert_eq!(
            tree.content("polymarket")
                .expect("ready summary should remain"),
            ready_summary("192.168.0.21:19590")
        );
    }

    #[test]
    fn publish_updates_stale_ready_summary_in_place() {
        let tree = SharedSrvTree::new();
        tree.set_ready_summary("polymarket", ready_summary("192.168.0.21:19591"));
        let before_id = tree.file_id("polymarket").expect("ready file id");
        let address = serve_tree(tree.clone());
        let mut publication = publication(&address);
        publication.vault_endpoint_bind = address;

        let outcome = publish_r9p_export(&publication).expect("publish should succeed");

        assert_eq!(outcome, PublishOutcome::Updated);
        assert_eq!(tree.file_id("polymarket"), Some(before_id));
        let descriptor = tree
            .content("polymarket")
            .expect("descriptor should be written");
        assert!(descriptor.contains("endpoint_bind\t192.168.0.21:19590\n"));
    }

    #[test]
    fn matching_summary_uses_vault_transport_class() {
        let publication = publication("127.0.0.1:9564");
        assert!(
            ready_summary_matches(&ready_summary("192.168.0.21:19590"), &publication)
                .expect("summary should compare")
        );
    }

    #[test]
    fn maintainer_wait_paths_use_srv_namespace() {
        assert_eq!(
            srv_wait_state_path("polymarket"),
            "/srv/wait/polymarket/state"
        );
        assert_eq!(
            srv_wait_state_path("kalshi/demo/actuator"),
            "/srv/wait/kalshi/demo/actuator/state"
        );
        assert_eq!(
            srv_wait_changed_after_path("polymarket", "token-1"),
            "/srv/wait/polymarket/changed-after/token-1"
        );
        assert_eq!(
            srv_wait_changed_after_path("kalshi/demo/actuator", "token-1"),
            "/srv/wait/kalshi/demo/actuator/changed-after/token-1"
        );
    }

    #[test]
    fn nested_srv_publication_uses_parent_path_and_leaf_name() {
        assert_eq!(
            srv_parent_and_leaf("kalshi/demo/actuator").expect("valid nested name"),
            ("/srv/kalshi/demo".to_string(), "actuator")
        );
    }

    #[test]
    fn srv_directory_at_registration_path_is_missing_registration() {
        let tree = SharedSrvTree::new();
        tree.ensure_srv_dir_path(&["kalshi", "demo", "actuator"]);
        let address = serve_tree(tree.clone());
        let mut client =
            Client::connect_tcp(&address, "codex", "/", 65_536).expect("connect test client");

        let state = inspect_srv_path(&mut client, "/srv/kalshi/demo/actuator")
            .expect("inspect should succeed");

        assert_eq!(state, SrvPathState::Missing);
    }

    #[test]
    fn publishes_missing_nested_srv_entry() {
        let tree = SharedSrvTree::new();
        tree.ensure_srv_dir_path(&["kalshi", "demo"]);
        let address = serve_tree(tree.clone());
        let mut publication = publication(&address);
        publication.vault_endpoint_bind = address;
        publication.service_name = "kalshi/demo/actuator".to_string();

        let outcome = publish_r9p_export(&publication).expect("publish should succeed");

        assert_eq!(outcome, PublishOutcome::Registered);
        let descriptor = tree
            .content_path(&["kalshi", "demo", "actuator"])
            .expect("descriptor should be written");
        assert!(descriptor.contains("format\tr9p-export.v1\n"));
        assert!(descriptor.contains("endpoint_bind\t192.168.0.21:19590\n"));
    }

    #[test]
    fn maintainer_republishes_disappeared_srv_entry() {
        let tree = SharedSrvTree::new();
        let address = serve_tree(tree.clone());
        let mut publication = publication(&address);
        publication.vault_endpoint_bind = address;

        let maintainer = maintain_r9p_export(
            publication,
            R9pExportMaintenanceConfig {
                retry_interval: Duration::from_secs(60),
            },
        )
        .expect("maintainer should start");
        tree.remove_file("polymarket");
        maintainer.reconcile_now();

        wait_for_descriptor(&tree, "polymarket");
        let status = maintainer.status();
        assert!(status.success_count >= 2);
        assert_eq!(status.last_error, None);
        maintainer.shutdown();
    }

    fn publication(vault_endpoint_bind: &str) -> R9pExportPublication {
        R9pExportPublication {
            vault_endpoint_bind: vault_endpoint_bind.to_string(),
            vault_uname: "codex".to_string(),
            vault_aname: "/".to_string(),
            service_name: "polymarket".to_string(),
            descriptor: ExportDescriptor {
                endpoint_bind: "192.168.0.21:19590".to_string(),
                aname: "/".to_string(),
                uname: "codex".to_string(),
                exported_root: "/".to_string(),
                transport_class: TransportClass::Tcp,
                mode: ExportMode::ReadOnly,
                auth: AuthBoundary::parse("wg:vault-runtime-lan").expect("auth should parse"),
                pid: 1234,
                protocol: Protocol::NineP2000,
                msize: 65_536,
                expires_at: None,
                local_root_label: Some("polymarket-watcher".to_string()),
                namespace_mount_paths: Vec::new(),
                extra_fields: BTreeMap::new(),
            },
        }
    }

    fn ready_summary(endpoint: &str) -> String {
        [
            "service: polymarket",
            "owner: codex.interface",
            "channel_kind: peer_namespace",
            "channel: r9p-export:polymarket",
            &format!(
                "endpoint: inline:r9p-export:polymarket:{endpoint}:codex:network_class:vault-runtime-lan"
            ),
            "aname: /",
            "exported_root: /",
            "created_at_ms: 1",
            "attached_at_ms: 2",
            "",
        ]
        .join("\n")
    }

    #[derive(Clone)]
    struct SharedSrvTree {
        inner: Arc<Mutex<SrvTree>>,
    }

    impl SharedSrvTree {
        fn new() -> Self {
            Self {
                inner: Arc::new(Mutex::new(SrvTree::new())),
            }
        }

        fn set_ready_summary(&self, name: &str, content: String) {
            self.inner
                .lock()
                .expect("tree lock")
                .set_file(name.as_bytes(), content.into_bytes());
        }

        fn content(&self, name: &str) -> Option<String> {
            self.inner
                .lock()
                .expect("tree lock")
                .file_content(name.as_bytes())
                .map(|bytes| String::from_utf8(bytes).expect("utf-8 content"))
        }

        fn file_id(&self, name: &str) -> Option<u64> {
            self.inner
                .lock()
                .expect("tree lock")
                .file_id(name.as_bytes())
        }

        fn remove_file(&self, name: &str) {
            self.inner
                .lock()
                .expect("tree lock")
                .remove_file(name.as_bytes());
        }

        fn ensure_srv_dir_path(&self, segments: &[&str]) {
            self.inner
                .lock()
                .expect("tree lock")
                .ensure_srv_dir_path(segments);
        }

        fn content_path(&self, segments: &[&str]) -> Option<String> {
            self.inner
                .lock()
                .expect("tree lock")
                .file_content_path(segments)
                .map(|bytes| String::from_utf8(bytes).expect("utf-8 content"))
        }
    }

    impl FileTree for SharedSrvTree {
        fn attach(&mut self, _fid: u32, _uname: &[u8], _aname: &[u8]) -> Result<Qid> {
            Ok(Qid::dir(ROOT))
        }

        fn walk(
            &mut self,
            _fid: u32,
            _newfid: u32,
            start: Qid,
            names: &[Vec<u8>],
        ) -> Result<Vec<Qid>> {
            self.inner
                .lock()
                .expect("tree lock")
                .walk(start.path, names)
        }

        fn open(&mut self, _fid: u32, qid: Qid, _mode: u8) -> Result<OpenFile> {
            Ok(OpenFile { qid, iounit: 0 })
        }

        fn read(&mut self, _fid: u32, qid: Qid, offset: u64, count: u32) -> Result<ReadData> {
            self.inner
                .lock()
                .expect("tree lock")
                .read(qid.path, offset, count)
        }

        fn stat(&mut self, qid: Qid) -> Result<Stat> {
            self.inner.lock().expect("tree lock").stat(qid.path)
        }

        fn create(
            &mut self,
            _fid: u32,
            qid: Qid,
            name: &[u8],
            _perm: u32,
            _mode: u8,
        ) -> Result<OpenFile> {
            self.inner.lock().expect("tree lock").create(qid.path, name)
        }

        fn write(&mut self, _fid: u32, qid: Qid, offset: u64, data: &[u8]) -> Result<u32> {
            self.inner
                .lock()
                .expect("tree lock")
                .write(qid.path, offset, data)
        }

        fn remove(&mut self, _fid: u32, qid: Qid) -> Result<()> {
            self.inner.lock().expect("tree lock").remove(qid.path)
        }
    }

    const ROOT: u64 = 1;
    const SRV: u64 = 2;

    struct SrvTree {
        nodes: BTreeMap<u64, TestNode>,
        next_id: u64,
    }

    struct TestNode {
        name: Vec<u8>,
        parent: u64,
        body: TestBody,
    }

    enum TestBody {
        Dir(BTreeMap<Vec<u8>, u64>),
        File(Vec<u8>),
    }

    impl SrvTree {
        fn new() -> Self {
            let mut nodes = BTreeMap::new();
            nodes.insert(
                ROOT,
                TestNode {
                    name: b".".to_vec(),
                    parent: ROOT,
                    body: TestBody::Dir(BTreeMap::from([(b"srv".to_vec(), SRV)])),
                },
            );
            nodes.insert(
                SRV,
                TestNode {
                    name: b"srv".to_vec(),
                    parent: ROOT,
                    body: TestBody::Dir(BTreeMap::new()),
                },
            );
            Self { nodes, next_id: 3 }
        }

        fn walk(&self, start: u64, names: &[Vec<u8>]) -> Result<Vec<Qid>> {
            let mut current = start;
            let mut qids = Vec::new();
            for name in names {
                if name == b"." {
                    qids.push(self.qid(current)?);
                    continue;
                }
                if name == b".." {
                    current = self.node(current)?.parent;
                    qids.push(self.qid(current)?);
                    continue;
                }
                let node = self.node(current)?;
                let TestBody::Dir(children) = &node.body else {
                    break;
                };
                let Some(next) = children.get(name).copied() else {
                    break;
                };
                current = next;
                qids.push(self.qid(current)?);
            }
            Ok(qids)
        }

        fn create(&mut self, parent: u64, name: &[u8]) -> Result<OpenFile> {
            let parent_node = self
                .nodes
                .get_mut(&parent)
                .ok_or_else(|| Error::from("missing parent"))?;
            let TestBody::Dir(children) = &mut parent_node.body else {
                return Err(Error::from("not a directory"));
            };
            if children.contains_key(name) {
                return Err(Error::from("file exists"));
            }
            let id = self.next_id;
            self.next_id += 1;
            children.insert(name.to_vec(), id);
            self.nodes.insert(
                id,
                TestNode {
                    name: name.to_vec(),
                    parent,
                    body: TestBody::File(Vec::new()),
                },
            );
            Ok(OpenFile {
                qid: Qid::file(id),
                iounit: 0,
            })
        }

        fn set_file(&mut self, name: &[u8], content: Vec<u8>) {
            if let Some(id) = self.child(SRV, name) {
                if let Some(TestNode {
                    body: TestBody::File(bytes),
                    ..
                }) = self.nodes.get_mut(&id)
                {
                    *bytes = content;
                    return;
                }
            }
            let id = self.next_id;
            self.next_id += 1;
            if let Some(TestNode {
                body: TestBody::Dir(children),
                ..
            }) = self.nodes.get_mut(&SRV)
            {
                children.insert(name.to_vec(), id);
            }
            self.nodes.insert(
                id,
                TestNode {
                    name: name.to_vec(),
                    parent: SRV,
                    body: TestBody::File(content),
                },
            );
        }

        fn ensure_srv_dir_path(&mut self, segments: &[&str]) {
            let mut current = SRV;
            for segment in segments {
                current = match self.child(current, segment.as_bytes()) {
                    Some(id) => id,
                    None => self.insert_dir(current, segment.as_bytes()),
                };
            }
        }

        fn insert_dir(&mut self, parent: u64, name: &[u8]) -> u64 {
            let id = self.next_id;
            self.next_id += 1;
            if let Some(TestNode {
                body: TestBody::Dir(children),
                ..
            }) = self.nodes.get_mut(&parent)
            {
                children.insert(name.to_vec(), id);
            }
            self.nodes.insert(
                id,
                TestNode {
                    name: name.to_vec(),
                    parent,
                    body: TestBody::Dir(BTreeMap::new()),
                },
            );
            id
        }

        fn file_content(&self, name: &[u8]) -> Option<Vec<u8>> {
            let id = self.child(SRV, name)?;
            match &self.nodes.get(&id)?.body {
                TestBody::File(bytes) => Some(bytes.clone()),
                TestBody::Dir(_) => None,
            }
        }

        fn file_content_path(&self, segments: &[&str]) -> Option<Vec<u8>> {
            let id = self.child_path(SRV, segments)?;
            match &self.nodes.get(&id)?.body {
                TestBody::File(bytes) => Some(bytes.clone()),
                TestBody::Dir(_) => None,
            }
        }

        fn file_id(&self, name: &[u8]) -> Option<u64> {
            self.child(SRV, name)
        }

        fn read(&self, id: u64, offset: u64, count: u32) -> Result<ReadData> {
            match &self.node(id)?.body {
                TestBody::File(bytes) => {
                    let offset = usize::try_from(offset).unwrap_or(usize::MAX);
                    let count = usize::try_from(count).unwrap_or(usize::MAX);
                    let end = offset.saturating_add(count).min(bytes.len());
                    Ok(ReadData::Bytes(if offset >= bytes.len() {
                        Vec::new()
                    } else {
                        bytes[offset..end].to_vec()
                    }))
                }
                TestBody::Dir(children) => Ok(ReadData::Directory(
                    children
                        .values()
                        .filter_map(|id| self.stat(*id).ok())
                        .collect(),
                )),
            }
        }

        fn write(&mut self, id: u64, offset: u64, data: &[u8]) -> Result<u32> {
            let node = self
                .nodes
                .get_mut(&id)
                .ok_or_else(|| Error::from("missing file"))?;
            let TestBody::File(bytes) = &mut node.body else {
                return Err(Error::from("not a file"));
            };
            let offset = usize::try_from(offset).map_err(|_| Error::from("offset overflow"))?;
            if bytes.len() < offset {
                bytes.resize(offset, 0);
            }
            if bytes.len() < offset + data.len() {
                bytes.resize(offset + data.len(), 0);
            }
            bytes[offset..offset + data.len()].copy_from_slice(data);
            u32::try_from(data.len()).map_err(|_| Error::from("write too large"))
        }

        fn remove(&mut self, id: u64) -> Result<()> {
            if id == ROOT || id == SRV {
                return Err(Error::from("cannot remove directory"));
            }
            let parent = self.node(id)?.parent;
            let name = self.node(id)?.name.clone();
            if let Some(TestNode {
                body: TestBody::Dir(children),
                ..
            }) = self.nodes.get_mut(&parent)
            {
                children.remove(&name);
            }
            self.nodes.remove(&id);
            Ok(())
        }

        fn remove_file(&mut self, name: &[u8]) {
            if let Some(id) = self.child(SRV, name) {
                let _ = self.remove(id);
            }
        }

        fn stat(&self, id: u64) -> Result<Stat> {
            let node = self.node(id)?;
            match &node.body {
                TestBody::Dir(_) => Ok(Stat::new(node.name.clone(), Qid::dir(id), DMDIR | 0o555)),
                TestBody::File(bytes) => {
                    let mut stat = Stat::new(node.name.clone(), Qid::file(id), 0o666);
                    stat.length = bytes.len() as u64;
                    Ok(stat)
                }
            }
        }

        fn qid(&self, id: u64) -> Result<Qid> {
            match self.node(id)?.body {
                TestBody::Dir(_) => Ok(Qid::dir(id)),
                TestBody::File(_) => Ok(Qid::file(id)),
            }
        }

        fn node(&self, id: u64) -> Result<&TestNode> {
            self.nodes
                .get(&id)
                .ok_or_else(|| Error::from("file does not exist"))
        }

        fn child(&self, parent: u64, name: &[u8]) -> Option<u64> {
            let TestBody::Dir(children) = &self.nodes.get(&parent)?.body else {
                return None;
            };
            children.get(name).copied()
        }

        fn child_path(&self, start: u64, segments: &[&str]) -> Option<u64> {
            let mut current = start;
            for segment in segments {
                current = self.child(current, segment.as_bytes())?;
            }
            Some(current)
        }
    }

    fn serve_tree(tree: SharedSrvTree) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let address = listener.local_addr().expect("local addr").to_string();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let connection_tree = tree.clone();
                thread::spawn(move || {
                    let _ = serve_connection(connection_tree, &mut stream);
                });
            }
        });
        address
    }

    fn wait_for_descriptor(tree: &SharedSrvTree, name: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if tree
                .content(name)
                .map(|content| content.contains("format\tr9p-export.v1\n"))
                .unwrap_or(false)
            {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("descriptor was not republished");
    }

    fn serve_connection(tree: SharedSrvTree, stream: &mut TcpStream) -> Result<()> {
        let mut server = Server::new(tree);
        loop {
            let mut prefix = [0_u8; 4];
            if stream.read_exact(&mut prefix).is_err() {
                return Ok(());
            }
            let size = u32::from_le_bytes(prefix);
            let rest_len = usize::try_from(size - 4).map_err(|_| Error::from("frame too large"))?;
            let mut frame = Vec::with_capacity(size as usize);
            frame.extend(prefix);
            frame.resize(size as usize, 0);
            stream
                .read_exact(&mut frame[4..4 + rest_len])
                .map_err(|error| Error::from(format!("read request: {error}")))?;
            let request = codec::decode_tmessage(&frame)?;
            let reply = match request {
                TMessage::Version { .. } => server.handle(request),
                _ => server.handle(request),
            };
            let encoded = codec::encode_rmessage(&reply)?;
            stream
                .write_all(&encoded)
                .map_err(|error| Error::from(format!("write reply: {error}")))?;
        }
    }
}
