//! Bounded coherent local materialization of one admitted 9P subtree.
//!
//! The materialization is a derived read view, not an authority or a second
//! source of truth. It subscribes to the source change stream before taking
//! its initial snapshot, applies ordinary file changes incrementally, and
//! falls back to a complete snapshot whenever coherence cannot be proven.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use crate::{
    feed::{
        scope_matches, start_feed_worker, FeedEvent, FeedEventBus, FeedEventReceiver, FeedState,
        FeedWake, FeedWorkerConfig, FeedWorkerHandle,
    },
    is_dir, is_symlink, Client, ClientSession, ConnectionConfig, DirEntry, Error, Result, OREAD,
};

mod local;

use local::LocalMaterialization;

const CHANGE_FEED_CAPACITY: usize = 4096;
const CHANGE_FEED_RECONNECT_DELAY: Duration = Duration::from_secs(1);
const MAXIMUM_ENTRIES: u64 = 65_536;
const MAXIMUM_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;
const MAXIMUM_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAXIMUM_DEPTH: u64 = 64;
const MAXIMUM_PARALLELISM: u64 = 32;
const MAXIMUM_IN_FLIGHT_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaterializationLimits {
    pub maximum_entries: u64,
    pub maximum_total_bytes: u64,
    pub maximum_file_bytes: u64,
    pub maximum_depth: u64,
    pub parallelism: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaterializationConfig {
    pub label: String,
    pub source: String,
    pub change_feed_catch_up: String,
    pub change_feed_stream: String,
    pub change_feed_cursor_template: String,
    pub change_scope: Option<String>,
    pub request_timeout: Duration,
    pub limits: MaterializationLimits,
}

/// One live local read view backed by an authenticated 9P session.
pub struct CoherentMaterialization {
    root: PathBuf,
    coherent: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    receiver_wake: crate::feed::FeedReceiverWake,
    session: ClientSession,
    feed: Option<FeedWorkerHandle>,
    worker: Option<JoinHandle<()>>,
}

#[derive(Debug)]
struct SnapshotPlan {
    directories: Vec<PathBuf>,
    files: Vec<RemoteFile>,
}

#[derive(Debug)]
struct RemoteFile {
    relative: PathBuf,
    remote: String,
}

enum RemoteNode {
    Directory,
    File(Vec<u8>),
}

impl MaterializationLimits {
    pub fn validate(&self) -> Result<()> {
        if self.maximum_entries == 0
            || self.maximum_entries > MAXIMUM_ENTRIES
            || self.maximum_total_bytes == 0
            || self.maximum_total_bytes > MAXIMUM_TOTAL_BYTES
            || self.maximum_file_bytes == 0
            || self.maximum_file_bytes > MAXIMUM_FILE_BYTES
            || self.maximum_file_bytes > self.maximum_total_bytes
            || self.maximum_depth == 0
            || self.maximum_depth > MAXIMUM_DEPTH
            || self.parallelism == 0
            || self.parallelism > MAXIMUM_PARALLELISM
            || self
                .maximum_file_bytes
                .checked_mul(self.parallelism)
                .map_or(true, |bytes| bytes > MAXIMUM_IN_FLIGHT_BYTES)
        {
            return Err(Error::new(
                libc::EINVAL,
                "local materialization limits are invalid",
            ));
        }
        Ok(())
    }
}

impl MaterializationConfig {
    fn validate(&self) -> Result<()> {
        if self.label.is_empty()
            || self.label.len() > 64
            || !self
                .label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            || self.request_timeout.is_zero()
            || self.request_timeout > Duration::from_secs(24 * 60 * 60)
        {
            return Err(Error::new(
                libc::EINVAL,
                "local materialization configuration is invalid",
            ));
        }
        validate_namespace_path(&self.source)?;
        validate_namespace_path(&self.change_feed_catch_up)?;
        validate_namespace_path(&self.change_feed_stream)?;
        if self
            .change_feed_cursor_template
            .match_indices("{event_id}")
            .count()
            != 1
        {
            return Err(Error::new(
                libc::EINVAL,
                "local materialization cursor template is invalid",
            ));
        }
        validate_namespace_path(
            &self
                .change_feed_cursor_template
                .replace("{event_id}", "event"),
        )?;
        if self.change_scope.as_deref().is_some_and(|scope| {
            scope.is_empty()
                || scope.len() > 256
                || scope
                    .bytes()
                    .any(|byte| byte == 0 || byte.is_ascii_control())
        }) {
            return Err(Error::new(
                libc::EINVAL,
                "local materialization change scope is invalid",
            ));
        }
        self.limits.validate()
    }
}

impl CoherentMaterialization {
    pub fn connect(
        local_root: &Path,
        connection: &ConnectionConfig,
        connect_timeout: Duration,
        config: MaterializationConfig,
    ) -> Result<Self> {
        if !local_root.is_absolute() {
            return Err(Error::new(
                libc::EINVAL,
                "local materialization root must be absolute",
            ));
        }
        config.validate()?;
        let cache = Arc::new(LocalMaterialization::prepare(local_root)?);
        let session = ClientSession::connect(connection, connect_timeout)?;
        let bus = FeedEventBus::new(CHANGE_FEED_CAPACITY);
        let mut receiver = bus.subscribe();
        let receiver_wake = receiver.wake_handle();
        let feed_wake = FeedWake::new();
        let observed_generation = feed_wake.generation()?;
        let feed = start_feed_worker(
            session.clone(),
            FeedWorkerConfig {
                path: config.change_feed_catch_up.clone(),
                stream_path: config.change_feed_stream.clone(),
                cursor_template: Some(config.change_feed_cursor_template.clone()),
                cache: None,
                event_bus: Some(bus.clone()),
                wake: Some(feed_wake.clone()),
                reconnect_delay: CHANGE_FEED_RECONNECT_DELAY,
                lookup_timeout: config.request_timeout,
                read_timeout: Duration::from_secs(24 * 60 * 60),
                control_timeout: config.request_timeout,
                backpressure_limit: CHANGE_FEED_CAPACITY,
            },
            FeedState::new(),
        )?;
        if let Err(error) = feed_wake.wait_after(observed_generation) {
            let _ = session.shutdown();
            feed.stop_and_join();
            return Err(error);
        }

        let startup = synchronize(&cache, &session, &config)
            .and_then(|()| drain_startup_events(&cache, &session, &config, &bus, &mut receiver));
        if let Err(error) = startup {
            let _ = session.shutdown();
            feed.stop_and_join();
            return Err(error);
        }

        let root = cache.tree().to_path_buf();
        let coherent = Arc::new(AtomicBool::new(true));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_coherent = Arc::clone(&coherent);
        let worker_stop = Arc::clone(&stop);
        let worker_session = session.clone();
        let worker_cache = Arc::clone(&cache);
        let worker_config = config.clone();
        let worker_bus = bus.clone();
        let worker_wake = feed_wake.clone();
        let worker = match thread::Builder::new()
            .name(format!("r9p-materialize-{}", config.label))
            .spawn(move || {
                event_loop(
                    worker_cache,
                    worker_session,
                    worker_config,
                    worker_bus,
                    worker_wake,
                    receiver,
                    worker_coherent,
                    worker_stop,
                );
            }) {
            Ok(worker) => worker,
            Err(error) => {
                let _ = session.shutdown();
                feed.stop_and_join();
                return Err(Error::io("spawn local materialization worker", error));
            }
        };

        Ok(Self {
            root,
            coherent,
            stop,
            receiver_wake,
            session,
            feed: Some(feed),
            worker: Some(worker),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn is_coherent(&self) -> bool {
        self.coherent.load(Ordering::Acquire)
    }
}

impl std::fmt::Debug for CoherentMaterialization {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CoherentMaterialization")
            .field("root", &self.root)
            .field("coherent", &self.is_coherent())
            .finish()
    }
}

impl Drop for CoherentMaterialization {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.receiver_wake.notify();
        let _ = self.session.shutdown();
        if let Some(feed) = self.feed.take() {
            feed.stop_and_join();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn event_loop(
    cache: Arc<LocalMaterialization>,
    session: ClientSession,
    config: MaterializationConfig,
    bus: FeedEventBus,
    wake: FeedWake,
    mut receiver: FeedEventReceiver,
    coherent: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
) {
    let mut resync = false;
    while !stop.load(Ordering::Acquire) {
        if resync {
            coherent.store(false, Ordering::Release);
            let observed = wake.generation().unwrap_or(0);
            match synchronize(&cache, &session, &config)
                .and_then(|()| drain_startup_events(&cache, &session, &config, &bus, &mut receiver))
            {
                Ok(()) => {
                    coherent.store(true, Ordering::Release);
                    resync = false;
                    continue;
                }
                Err(error) => {
                    eprintln!(
                        "r9p: local materialization {} degraded: {}",
                        config.label,
                        error.message()
                    );
                    if stop.load(Ordering::Acquire) || wake.wait_after(observed).is_err() {
                        break;
                    }
                    continue;
                }
            }
        }

        match receiver.recv_until_stopped(&stop) {
            Ok(Some(event)) => {
                if event_requires_resync(&event) {
                    resync = true;
                    continue;
                }
                if !event_is_in_scope(&config, &event) {
                    continue;
                }
                coherent.store(false, Ordering::Release);
                match apply_event(&cache, &session, &config, event) {
                    Ok(()) => coherent.store(true, Ordering::Release),
                    Err(_) => resync = true,
                }
            }
            Ok(None) => break,
            Err(_) => {
                receiver = bus.subscribe();
                resync = true;
            }
        }
    }
    coherent.store(false, Ordering::Release);
}

fn drain_startup_events(
    cache: &LocalMaterialization,
    session: &ClientSession,
    config: &MaterializationConfig,
    bus: &FeedEventBus,
    receiver: &mut FeedEventReceiver,
) -> Result<()> {
    loop {
        match receiver.recv_timeout(Duration::ZERO) {
            Ok(event) if event_requires_resync(&event) => synchronize(cache, session, config)?,
            Ok(event) if !event_is_in_scope(config, &event) => {}
            Ok(event) => apply_event(cache, session, config, event)?,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => return Ok(()),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                *receiver = bus.subscribe();
                synchronize(cache, session, config)?;
            }
        }
    }
}

fn event_requires_resync(event: &FeedEvent) -> bool {
    match event {
        FeedEvent::CoarseInvalidation { .. } => true,
        FeedEvent::Change { change, .. } => {
            change.change_kind == "resync" || change.change_kind == "renamed"
        }
    }
}

fn event_is_in_scope(config: &MaterializationConfig, event: &FeedEvent) -> bool {
    match event {
        FeedEvent::CoarseInvalidation { .. } => true,
        FeedEvent::Change { change, .. } => {
            scope_matches(config.change_scope.as_deref(), &change.scope)
        }
    }
}

fn apply_event(
    cache: &LocalMaterialization,
    session: &ClientSession,
    config: &MaterializationConfig,
    event: FeedEvent,
) -> Result<()> {
    let FeedEvent::Change { change, .. } = event else {
        return Err(Error::new(
            libc::EAGAIN,
            "local materialization requires resynchronization",
        ));
    };
    let relative = relative_change_path(&change.path)?;
    match change.change_kind.as_str() {
        "removed" => cache.remove(&relative),
        "created" | "modified" => {
            let remote = append_namespace_path(&config.source, &change.path)?;
            match read_remote_node(session, &remote, &config.limits, config.request_timeout) {
                Ok(RemoteNode::File(bytes)) => {
                    cache.replace_file(&relative, &bytes, &config.limits)
                }
                Ok(RemoteNode::Directory) => Err(Error::new(
                    libc::EAGAIN,
                    "directory change requires local materialization resynchronization",
                )),
                Err(error) if error.errno == libc::ENOENT => cache.remove(&relative),
                Err(error) => Err(error),
            }
        }
        _ => Err(Error::new(
            libc::EPROTO,
            "local materialization change record is invalid",
        )),
    }
}

fn synchronize(
    cache: &LocalMaterialization,
    session: &ClientSession,
    config: &MaterializationConfig,
) -> Result<()> {
    let staging = cache.create_staging()?;
    let result = discover_snapshot(session, config).and_then(|plan| {
        for directory in &plan.directories {
            cache.create_staged_directory(&staging, directory)?;
        }
        populate_snapshot(cache, &staging, session, config, plan.files)?;
        cache.publish_snapshot(&staging, &config.limits)
    });
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

fn discover_snapshot(
    session: &ClientSession,
    config: &MaterializationConfig,
) -> Result<SnapshotPlan> {
    let root = session.snapshot()?;
    let root_stat = root.stat_path_timeout(&config.source, config.request_timeout)?;
    if !is_dir(&root_stat) || is_symlink(&root_stat) {
        return Err(Error::new(
            libc::ENOTDIR,
            "local materialization source is not a plain directory",
        ));
    }

    let mut directories = Vec::new();
    let mut files = Vec::new();
    let mut pending = VecDeque::from([(PathBuf::new(), config.source.clone(), 0_u64)]);
    let mut entries = 0_u64;
    let mut total_bytes = 0_u64;
    while let Some((relative_parent, remote_parent, depth)) = pending.pop_front() {
        let mut children = list_remote_directory(session, &remote_parent, config.request_timeout)?;
        children.sort_by(|left, right| left.name.cmp(&right.name));
        for child in children {
            entries = entries.saturating_add(1);
            if entries > config.limits.maximum_entries {
                return Err(Error::new(
                    libc::EFBIG,
                    "local materialization entry bound exceeded",
                ));
            }
            let name = namespace_component(&child.name)?;
            let mut relative = relative_parent.clone();
            relative.push(name);
            let remote = append_namespace_path(&remote_parent, &format!("/{name}"))?;
            if is_symlink(&child.stat) {
                return Err(Error::new(
                    libc::EPERM,
                    "local materialization rejects symlinks",
                ));
            }
            if is_dir(&child.stat) {
                let child_depth = depth.saturating_add(1);
                if child_depth > config.limits.maximum_depth {
                    return Err(Error::new(
                        libc::EFBIG,
                        "local materialization depth bound exceeded",
                    ));
                }
                directories.push(relative.clone());
                pending.push_back((relative, remote, child_depth));
            } else {
                if child.stat.length > config.limits.maximum_file_bytes {
                    return Err(Error::new(
                        libc::EFBIG,
                        "local materialization file bound exceeded",
                    ));
                }
                total_bytes = total_bytes.checked_add(child.stat.length).ok_or_else(|| {
                    Error::new(
                        libc::EFBIG,
                        "local materialization total byte bound exceeded",
                    )
                })?;
                if total_bytes > config.limits.maximum_total_bytes {
                    return Err(Error::new(
                        libc::EFBIG,
                        "local materialization total byte bound exceeded",
                    ));
                }
                files.push(RemoteFile { relative, remote });
            }
        }
    }
    Ok(SnapshotPlan { directories, files })
}

fn populate_snapshot(
    cache: &LocalMaterialization,
    staging: &Path,
    session: &ClientSession,
    config: &MaterializationConfig,
    files: Vec<RemoteFile>,
) -> Result<()> {
    let jobs = Arc::new(Mutex::new(VecDeque::from(files)));
    let failure = Arc::new(Mutex::new(None::<Error>));
    let actual_bytes = Arc::new(AtomicU64::new(0));
    let workers = usize::try_from(config.limits.parallelism)
        .map_err(|_| Error::new(libc::EINVAL, "local materialization parallelism is invalid"))?;
    thread::scope(|scope| {
        for _ in 0..workers {
            let jobs = Arc::clone(&jobs);
            let failure = Arc::clone(&failure);
            let actual_bytes = Arc::clone(&actual_bytes);
            scope.spawn(move || loop {
                if failure.lock().map_or(true, |failure| failure.is_some()) {
                    return;
                }
                let job = match jobs.lock() {
                    Ok(mut jobs) => jobs.pop_front(),
                    Err(_) => {
                        set_failure(
                            &failure,
                            Error::new(libc::EIO, "local materialization queue lock poisoned"),
                        );
                        return;
                    }
                };
                let Some(job) = job else {
                    return;
                };
                let result =
                    read_remote_node(session, &job.remote, &config.limits, config.request_timeout)
                        .and_then(|node| match node {
                            RemoteNode::Directory => Err(Error::new(
                                libc::EAGAIN,
                                "local materialization source changed during snapshot",
                            )),
                            RemoteNode::File(bytes) => {
                                let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                                let previous = actual_bytes.fetch_add(length, Ordering::AcqRel);
                                if previous.saturating_add(length)
                                    > config.limits.maximum_total_bytes
                                {
                                    return Err(Error::new(
                                        libc::EFBIG,
                                        "local materialization total byte bound exceeded",
                                    ));
                                }
                                cache.write_staged_file(staging, &job.relative, &bytes)
                            }
                        });
                if let Err(error) = result {
                    set_failure(&failure, error);
                    return;
                }
            });
        }
    });
    let outcome = failure
        .lock()
        .map_err(|_| Error::new(libc::EIO, "local materialization queue lock poisoned"))?
        .take()
        .map_or(Ok(()), Err);
    outcome
}

fn set_failure(failure: &Mutex<Option<Error>>, error: Error) {
    if let Ok(mut failure) = failure.lock() {
        if failure.is_none() {
            *failure = Some(error);
        }
    }
}

fn read_remote_node(
    session: &ClientSession,
    path: &str,
    limits: &MaterializationLimits,
    request_timeout: Duration,
) -> Result<RemoteNode> {
    let client = session.snapshot()?;
    let stat = client.stat_path_timeout(path, request_timeout)?;
    if is_symlink(&stat) {
        return Err(Error::new(
            libc::EPERM,
            "local materialization rejects symlinks",
        ));
    }
    if is_dir(&stat) {
        return Ok(RemoteNode::Directory);
    }
    if stat.length > limits.maximum_file_bytes {
        return Err(Error::new(
            libc::EFBIG,
            "local materialization file exceeds its bound",
        ));
    }
    let maximum = limits
        .maximum_file_bytes
        .checked_add(1)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| Error::new(libc::EOVERFLOW, "local materialization file bound invalid"))?;
    let bytes = client.read_path_timeout(path, maximum, request_timeout)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limits.maximum_file_bytes {
        return Err(Error::new(
            libc::EFBIG,
            "local materialization file exceeds its bound",
        ));
    }
    Ok(RemoteNode::File(bytes))
}

fn list_remote_directory(
    session: &ClientSession,
    path: &str,
    request_timeout: Duration,
) -> Result<Vec<DirEntry>> {
    let client = session.snapshot()?;
    list_remote_directory_with_client(&client, path, request_timeout)
}

fn list_remote_directory_with_client(
    client: &Client,
    path: &str,
    request_timeout: Duration,
) -> Result<Vec<DirEntry>> {
    let fid = client.walk_path_timeout(path, request_timeout)?;
    let result = (|| {
        let stat = client.stat_timeout(fid, request_timeout)?;
        if !is_dir(&stat) || is_symlink(&stat) {
            return Err(Error::new(
                libc::ENOTDIR,
                "local materialization source is not a plain directory",
            ));
        }
        client.open_timeout(fid, OREAD, request_timeout)?;
        crate::read_open_directory_entries(client, fid, request_timeout)
    })();
    let clunk = client.clunk_timeout(fid, request_timeout);
    match (result, clunk) {
        (Ok(entries), Ok(())) => Ok(entries),
        (Ok(_), Err(error)) | (Err(error), _) => Err(error),
    }
}

fn append_namespace_path(root: &str, suffix: &str) -> Result<String> {
    if !root.starts_with('/')
        || root == "/"
        || root.ends_with('/')
        || !suffix.starts_with('/')
        || suffix == "/"
        || suffix.contains("//")
    {
        return Err(Error::new(
            libc::EINVAL,
            "local materialization namespace path is invalid",
        ));
    }
    let path = format!("{root}{suffix}");
    if path.len() > 2048 {
        return Err(Error::new(
            libc::ENAMETOOLONG,
            "local materialization namespace path is too long",
        ));
    }
    validate_namespace_path(&path)?;
    Ok(path)
}

fn relative_change_path(path: &str) -> Result<PathBuf> {
    if !path.starts_with('/') || path == "/" || path.ends_with('/') || path.contains("//") {
        return Err(Error::new(
            libc::EPROTO,
            "local materialization change path is invalid",
        ));
    }
    let mut relative = PathBuf::new();
    for component in path[1..].split('/') {
        if !valid_component(component) {
            return Err(Error::new(
                libc::EPROTO,
                "local materialization change path is invalid",
            ));
        }
        relative.push(component);
    }
    Ok(relative)
}

fn namespace_component(bytes: &[u8]) -> Result<&str> {
    let value = std::str::from_utf8(bytes)
        .map_err(|_| Error::new(libc::EILSEQ, "local materialization name is not UTF-8"))?;
    if !valid_component(value) {
        return Err(Error::new(
            libc::EPROTO,
            "local materialization name is invalid",
        ));
    }
    Ok(value)
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value
            .bytes()
            .any(|byte| byte == b'/' || byte == 0 || byte.is_ascii_control())
}

fn validate_namespace_path(path: &str) -> Result<()> {
    if !path.starts_with('/') || path == "/" || path.len() > 2048 {
        return Err(Error::new(
            libc::EINVAL,
            "local materialization namespace path is invalid",
        ));
    }
    crate::parse_namespace_path(path.as_bytes()).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> MaterializationLimits {
        MaterializationLimits {
            maximum_entries: 16_384,
            maximum_total_bytes: 256 * 1024 * 1024,
            maximum_file_bytes: 4 * 1024 * 1024,
            maximum_depth: 32,
            parallelism: 16,
        }
    }

    #[test]
    fn materialization_limits_bound_memory_and_traversal() {
        let mut configured = limits();
        assert!(configured.validate().is_ok());
        configured.parallelism = 33;
        assert!(configured.validate().is_err());
        configured.parallelism = 16;
        configured.maximum_file_bytes = 16 * 1024 * 1024;
        assert!(configured.validate().is_err());
    }

    #[test]
    fn change_paths_are_strict_relative_paths() {
        assert_eq!(
            relative_change_path("/arch/example.md").expect("valid path"),
            PathBuf::from("arch/example.md")
        );
        assert!(relative_change_path("/").is_err());
        assert!(relative_change_path("/../example.md").is_err());
        assert!(relative_change_path("/bad\nname.md").is_err());
    }

    #[test]
    fn configuration_accepts_logical_coordinator_paths() {
        let config = MaterializationConfig {
            label: "memory".to_string(),
            source: "/memory/personal/wiki/entries".to_string(),
            change_feed_catch_up: "/memory/personal/changes/recent".to_string(),
            change_feed_stream: "/memory/personal/changes/stream".to_string(),
            change_feed_cursor_template: "/memory/personal/changes/after/{event_id}".to_string(),
            change_scope: Some("shared".to_string()),
            request_timeout: Duration::from_secs(5),
            limits: limits(),
        };
        assert!(config.validate().is_ok());
    }
}
