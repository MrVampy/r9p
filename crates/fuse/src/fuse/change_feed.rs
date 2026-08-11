//! Runtime namespace-change feed consumer.
//!
//! The mount consumes generic path-change records only. Application-specific
//! domain events are projected into this shape before they reach this Rust
//! mechanism.

use super::{
    invalidation::{notify_kernel_invalidations, KernelInvalidation},
    util::is_transport_error,
    R9pFuse,
};
use crate::error::{Error, Result};
use r9p::fid::Fid;
use session::{
    feed::{
        feed_catch_up_path, parse_namespace_change_record, parse_namespace_path, scope_matches,
        select_feed_records, FeedEvent, FeedEventReceiver, FeedReceiverWake, NamespaceChange,
    },
    Client, OREAD,
};
use std::{
    fs::File,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

pub(super) const DEFAULT_CHANGE_FEED_BACKPRESSURE_LIMIT: usize = 4096;

pub(super) struct ChangeFeedHandle {
    stop: Arc<AtomicBool>,
    receiver_wake: Option<FeedReceiverWake>,
    direct_control: Option<Arc<DirectFeedControl>>,
    handle: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct DirectFeedControl {
    wait: Mutex<()>,
    changed: Condvar,
    client: Mutex<Option<Client>>,
}

impl ChangeFeedHandle {
    pub(super) fn stop_and_join(mut self) {
        self.signal_stop();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }

    fn signal_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(wake) = &self.receiver_wake {
            wake.notify();
        }
        if let Some(control) = &self.direct_control {
            control.stop();
        }
    }
}

impl Drop for ChangeFeedHandle {
    fn drop(&mut self) {
        self.signal_stop();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl DirectFeedControl {
    fn install_client(&self, client: Client, stop: &AtomicBool) -> Result<()> {
        if stop.load(Ordering::SeqCst) {
            let _ = client.shutdown();
            return Err(Error::new(libc::ESHUTDOWN, "change feed stopped"));
        }
        let mut slot = self
            .client
            .lock()
            .map_err(|_| Error::new(libc::EIO, "change feed client lock poisoned"))?;
        if stop.load(Ordering::SeqCst) {
            drop(slot);
            let _ = client.shutdown();
            return Err(Error::new(libc::ESHUTDOWN, "change feed stopped"));
        }
        *slot = Some(client);
        Ok(())
    }

    fn clear_client(&self) {
        if let Ok(mut slot) = self.client.lock() {
            *slot = None;
        }
    }

    fn stop(&self) {
        let client = self.client.lock().ok().and_then(|mut slot| slot.take());
        if let Some(client) = client {
            let _ = client.shutdown();
        }
        self.changed.notify_all();
    }

    fn wait_reconnect(&self, duration: Duration, stop: &AtomicBool) {
        if duration.is_zero() || stop.load(Ordering::SeqCst) {
            return;
        }
        let Ok(guard) = self.wait.lock() else {
            return;
        };
        let _wait_result = self
            .changed
            .wait_timeout_while(guard, duration, |_| !stop.load(Ordering::SeqCst));
    }
}

impl R9pFuse {
    pub(super) fn start_change_feed(&self, file: &File) -> Result<Option<ChangeFeedHandle>> {
        let Some(path) = self.config.change_feed_path.clone() else {
            self.status.set_change_feed("disabled", None, None, None);
            return Ok(None);
        };
        let stream_path = self.config.change_feed_stream_path.clone().ok_or_else(|| {
            Error::new(libc::EINVAL, "change feed requires a blocking stream path")
        })?;
        let mut file = file
            .try_clone()
            .map_err(|error| Error::io("clone /dev/fuse for change feed", error))?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let direct_control = Arc::new(DirectFeedControl::default());
        let thread_control = Arc::clone(&direct_control);
        let mut fs = self.clone();
        let handle = thread::Builder::new()
            .name("r9p-fuse-change-feed".to_string())
            .spawn(move || {
                change_feed_loop(
                    &mut fs,
                    &mut file,
                    path,
                    stream_path,
                    thread_stop,
                    thread_control,
                )
            })
            .map_err(|error| Error::io("spawn namespace change-feed consumer", error))?;
        Ok(Some(ChangeFeedHandle {
            stop,
            receiver_wake: None,
            direct_control: Some(direct_control),
            handle: Some(handle),
        }))
    }

    pub(super) fn start_session_feed_events(
        &self,
        file: &File,
        receiver: FeedEventReceiver,
    ) -> Result<Option<ChangeFeedHandle>> {
        let mut file = file
            .try_clone()
            .map_err(|error| Error::io("clone /dev/fuse for session feed events", error))?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let receiver_wake = receiver.wake_handle();
        let mut fs = self.clone();
        let handle = thread::Builder::new()
            .name("r9p-fuse-session-feed".to_string())
            .spawn(move || session_feed_event_loop(&mut fs, &mut file, receiver, thread_stop))
            .map_err(|error| Error::io("spawn session feed projection", error))?;
        Ok(Some(ChangeFeedHandle {
            stop,
            receiver_wake: Some(receiver_wake),
            direct_control: None,
            handle: Some(handle),
        }))
    }

    fn apply_namespace_change(
        &mut self,
        file: &mut File,
        change: NamespaceChange,
        source: &'static str,
    ) -> Result<()> {
        if !scope_matches(self.config.change_feed_scope.as_deref(), &change.scope) {
            return Ok(());
        }
        if change.change_kind == "resync" {
            self.apply_coarse_invalidation(file, "change feed requested resynchronization");
            self.status
                .set_change_feed("connected", Some(source), Some(change.event_id), None);
            return Ok(());
        }
        let path = parse_namespace_path(&change.path)?;
        let old_path = change
            .old_path
            .as_deref()
            .map(parse_namespace_path)
            .transpose()?;
        let invalidation = {
            let mut nodes = self.nodes()?;
            match change.change_kind.as_str() {
                "created" => KernelInvalidation::path(
                    nodes.mark_path_stale(&path),
                    nodes
                        .mark_parent_directory_cache_stale(&path)
                        .into_iter()
                        .collect(),
                ),
                "removed" => KernelInvalidation::path(
                    nodes.mark_path_prefix_stale(&path),
                    nodes
                        .mark_parent_directory_cache_stale(&path)
                        .into_iter()
                        .collect(),
                ),
                "renamed" => {
                    let mut stale = old_path
                        .as_deref()
                        .map(|old| nodes.mark_path_prefix_stale(old))
                        .unwrap_or_default();
                    stale.extend(nodes.mark_path_prefix_stale(&path));
                    let mut parent_entries = Vec::new();
                    if let Some(old) = old_path.as_deref() {
                        parent_entries.extend(nodes.mark_parent_directory_cache_stale(old));
                    }
                    parent_entries.extend(nodes.mark_parent_directory_cache_stale(&path));
                    KernelInvalidation::path(stale, parent_entries)
                }
                "modified" => KernelInvalidation::path(
                    nodes.mark_path_stale(&path),
                    nodes
                        .mark_parent_directory_cache_stale(&path)
                        .into_iter()
                        .collect(),
                ),
                _ => {
                    return Err(Error::new(
                        libc::EINVAL,
                        format!("unknown namespace change kind {}", change.change_kind),
                    ));
                }
            }
        };
        notify_kernel_invalidations(file, &invalidation);
        self.clunk_stale_bindings(invalidation.stale_bindings);
        self.status
            .set_change_feed("connected", Some(source), Some(change.event_id), None);
        Ok(())
    }

    fn apply_coarse_invalidation(&mut self, file: &mut File, reason: &str) {
        let stale = self
            .nodes()
            .map(|mut nodes| nodes.mark_path_bindings_stale())
            .unwrap_or_default();
        let invalidation = KernelInvalidation::coarse(stale);
        notify_kernel_invalidations(file, &invalidation);
        // Feed degradation only means cache precision is lost. Mark future
        // path-backed operations for rebind, but do not clunk the old fids out
        // from under concurrent kernel requests on the data client.
        self.record_mount_diagnostic("change_feed_coarse_invalidation", 0, reason);
    }
}

fn session_feed_event_loop(
    fs: &mut R9pFuse,
    file: &mut File,
    receiver: FeedEventReceiver,
    stop: Arc<AtomicBool>,
) {
    fs.status
        .set_change_feed("connected", Some("session"), None, None);
    loop {
        match receiver.recv_until_stopped(&stop) {
            Ok(Some(FeedEvent::Change { change, source })) => {
                if let Err(error) = fs.apply_namespace_change(file, change, source) {
                    fs.status.set_change_feed(
                        "degraded",
                        Some(source),
                        None,
                        Some(error.message().to_string()),
                    );
                    fs.record_mount_diagnostic(
                        "session_feed_event_failed",
                        error.errno,
                        error.message(),
                    );
                    fs.apply_coarse_invalidation(file, "session feed event failed");
                }
            }
            Ok(Some(FeedEvent::CoarseInvalidation { reason })) => {
                fs.status
                    .set_change_feed("degraded", Some("session"), None, Some(reason.clone()));
                fs.apply_coarse_invalidation(file, &reason);
            }
            Ok(None) => return,
            Err(_) => {
                fs.status.set_change_feed(
                    "degraded",
                    Some("session"),
                    None,
                    Some("session feed event bus disconnected".to_string()),
                );
                fs.apply_coarse_invalidation(file, "session feed event bus disconnected");
                return;
            }
        }
    }
}

fn change_feed_loop(
    fs: &mut R9pFuse,
    file: &mut File,
    path: String,
    stream_path: String,
    stop: Arc<AtomicBool>,
    control: Arc<DirectFeedControl>,
) {
    fs.status.set_change_feed("connecting", None, None, None);
    let mut feed_client = None;
    let mut data_client_stale = false;
    let mut since_event_id = None;
    while !stop.load(Ordering::SeqCst) {
        let client = match change_feed_client(fs, &mut feed_client, &stop, &control) {
            Ok(client) => client,
            Err(error) => {
                data_client_stale = true;
                fs.status.set_change_feed(
                    "degraded",
                    None,
                    None,
                    Some(error.message().to_string()),
                );
                fs.record_mount_diagnostic(
                    "change_feed_disconnected",
                    error.errno,
                    error.message(),
                );
                fs.apply_coarse_invalidation(file, "change feed degraded");
                control.wait_reconnect(fs.config.change_feed_reconnect_delay, &stop);
                continue;
            }
        };
        if data_client_stale {
            match fs.reconnect() {
                Ok(()) => {
                    data_client_stale = false;
                    fs.record_mount_diagnostic("change_feed_data_reconnect", 0, "reconnected");
                }
                Err(error) => {
                    fs.status.set_change_feed(
                        "degraded",
                        None,
                        None,
                        Some(error.message().to_string()),
                    );
                    fs.record_mount_diagnostic(
                        "change_feed_data_reconnect_failed",
                        error.errno,
                        error.message(),
                    );
                    fs.apply_coarse_invalidation(file, "change feed data reconnect failed");
                    control.wait_reconnect(fs.config.change_feed_reconnect_delay, &stop);
                    continue;
                }
            }
        }
        // Subscribe before taking the catch-up snapshot. A service can bind
        // the stream fid to its current cursor at open time, which closes the
        // otherwise unavoidable gap between reading a snapshot and opening a
        // live stream.
        let stream_fid = match open_feed(&client, &stream_path, fs.lookup_timeout()) {
            Ok(fid) => fid,
            Err(error) => {
                fs.status.set_change_feed(
                    "degraded",
                    Some("stream"),
                    None,
                    Some(error.message().to_string()),
                );
                fs.record_mount_diagnostic(
                    "change_feed_stream_subscribe_failed",
                    error.errno,
                    error.message(),
                );
                fs.apply_coarse_invalidation(file, "change feed subscription degraded");
                if feed_error_requires_data_reconnect(&error) {
                    feed_client = None;
                    control.clear_client();
                    data_client_stale = true;
                }
                control.wait_reconnect(fs.config.change_feed_reconnect_delay, &stop);
                continue;
            }
        };
        if since_event_id.is_some() {
            match consume_catch_up_once(fs, file, &path, &client, &mut since_event_id) {
                Ok(()) => {}
                Err(error) if is_feed_read_timeout(&error) => {}
                Err(error) => {
                    fs.status.set_change_feed(
                        "degraded",
                        Some("catch_up"),
                        None,
                        Some(error.message().to_string()),
                    );
                    fs.record_mount_diagnostic(
                        "change_feed_catch_up_failed",
                        error.errno,
                        error.message(),
                    );
                    fs.apply_coarse_invalidation(file, "change feed catch-up degraded");
                    if feed_error_requires_data_reconnect(&error) {
                        let _ = client.clunk_timeout(stream_fid, fs.control_timeout());
                        feed_client = None;
                        control.clear_client();
                        data_client_stale = true;
                        control.wait_reconnect(fs.config.change_feed_reconnect_delay, &stop);
                        continue;
                    }
                }
            }
        }
        match consume_stream_until_error(fs, file, &stop, &client, stream_fid, &mut since_event_id)
        {
            Ok(()) => break,
            Err(error) => {
                fs.status.set_change_feed(
                    "degraded",
                    Some("stream"),
                    None,
                    Some(error.message().to_string()),
                );
                fs.record_mount_diagnostic(
                    "change_feed_stream_disconnected",
                    error.errno,
                    error.message(),
                );
                fs.apply_coarse_invalidation(file, "change feed stream degraded");
                if feed_error_requires_data_reconnect(&error) {
                    feed_client = None;
                    control.clear_client();
                    data_client_stale = true;
                }
            }
        }
        control.wait_reconnect(fs.config.change_feed_reconnect_delay, &stop);
    }
}

fn change_feed_client(
    fs: &R9pFuse,
    slot: &mut Option<Client>,
    stop: &AtomicBool,
    control: &DirectFeedControl,
) -> Result<Client> {
    if let Some(client) = slot {
        return Ok(client.clone());
    }
    let client = Client::connect_with_timeout(&fs.config.connection(), fs.config.connect_timeout)?;
    control.install_client(client.clone(), stop)?;
    *slot = Some(client.clone());
    Ok(client)
}

fn consume_catch_up_once(
    fs: &mut R9pFuse,
    file: &mut File,
    path: &str,
    client: &Client,
    since_event_id: &mut Option<String>,
) -> Result<()> {
    fs.status.set_change_feed("connecting", None, None, None);
    let catch_up_path = feed_catch_up_path(
        path,
        since_event_id.as_deref(),
        fs.config.change_feed_cursor_template.as_deref(),
    );
    let fid = match open_feed(client, &catch_up_path, fs.lookup_timeout()) {
        Ok(fid) => fid,
        Err(error) if is_feed_read_timeout(&error) => {
            fs.status
                .set_change_feed("connected", Some("catch_up"), None, None);
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    match client.read_timeout(fid, 0, 64 * 1024, fs.config.change_feed_read_timeout) {
        Ok(data) if data.is_empty() => {
            let _ = client.clunk_timeout(fid, fs.control_timeout());
            fs.status
                .set_change_feed("connected", Some("catch_up"), None, None);
        }
        Ok(data) => {
            let _ = client.clunk_timeout(fid, fs.control_timeout());
            if let Some(event_id) =
                apply_feed_chunk(fs, file, &data, FeedReadMode::CatchUp { since_event_id })?
            {
                *since_event_id = Some(event_id);
            }
            fs.status
                .set_change_feed("connected", Some("catch_up"), None, None);
        }
        Err(error) if error.errno == libc::ETIMEDOUT => {
            let _ = client.clunk_timeout(fid, fs.control_timeout());
            fs.status
                .set_change_feed("connected", Some("catch_up"), None, None);
        }
        Err(error) => {
            let _ = client.clunk_timeout(fid, fs.control_timeout());
            return Err(error.into());
        }
    }
    Ok(())
}

fn consume_stream_until_error(
    fs: &mut R9pFuse,
    file: &mut File,
    stop: &AtomicBool,
    client: &Client,
    fid: Fid,
    since_event_id: &mut Option<String>,
) -> Result<()> {
    fs.status.set_change_feed("connecting", None, None, None);
    let result = loop {
        if stop.load(Ordering::SeqCst) {
            break Ok(());
        }
        match client.read_timeout(fid, 0, 64 * 1024, fs.config.change_feed_read_timeout) {
            Ok(data) if data.is_empty() => {
                fs.status
                    .set_change_feed("connected", Some("stream"), None, None);
            }
            Ok(data) => {
                match apply_feed_chunk(fs, file, &data, FeedReadMode::Stream) {
                    Ok(Some(event_id)) => *since_event_id = Some(event_id),
                    Ok(None) => {}
                    Err(error) => break Err(error),
                }
                fs.status
                    .set_change_feed("connected", Some("stream"), None, None);
            }
            Err(error) if error.errno == libc::ETIMEDOUT => {
                fs.status
                    .set_change_feed("connected", Some("stream"), None, None);
            }
            Err(error) => {
                break Err(error.into());
            }
        }
    };
    let _ = client.clunk_timeout(fid, fs.control_timeout());
    result
}

fn is_feed_read_timeout(error: &Error) -> bool {
    error.errno == libc::ETIMEDOUT
}

fn feed_error_requires_data_reconnect(error: &Error) -> bool {
    is_transport_error(error) && !is_feed_read_timeout(error)
}

fn open_feed(client: &Client, path: &str, timeout: Duration) -> Result<Fid> {
    let segments = parse_namespace_path(path)?;
    let fid = client.walk_timeout(client.root_fid(), &segments, timeout)?;
    if let Err(error) = client.open_timeout(fid, OREAD, timeout) {
        let _ = client.clunk_timeout(fid, timeout);
        return Err(error.into());
    }
    Ok(fid)
}

enum FeedReadMode<'a> {
    Stream,
    CatchUp {
        since_event_id: &'a mut Option<String>,
    },
}

impl FeedReadMode<'_> {
    fn source(&self) -> &'static str {
        match self {
            FeedReadMode::Stream => "stream",
            FeedReadMode::CatchUp { .. } => "catch_up",
        }
    }
}

fn apply_feed_chunk(
    fs: &mut R9pFuse,
    file: &mut File,
    data: &[u8],
    mode: FeedReadMode<'_>,
) -> Result<Option<String>> {
    let source = mode.source();
    let text = String::from_utf8_lossy(data);
    let records = text
        .lines()
        .filter_map(parse_namespace_change_record)
        .collect::<Vec<_>>();
    let (records, cursor_advanced_to) = match mode {
        FeedReadMode::Stream => (records, None),
        FeedReadMode::CatchUp { since_event_id } => {
            let selected = select_feed_records(
                records,
                since_event_id.as_ref().map(String::as_str),
                fs.config.change_feed_cursor_template.is_some(),
            );
            if selected.cursor_missed {
                fs.apply_coarse_invalidation(file, "change feed cursor fell outside recent window");
            }
            (selected.records, selected.cursor_advanced_to)
        }
    };
    if records.len() > fs.config.change_feed_backpressure_limit {
        let last_event_id = records.last().map(|record| record.event_id.clone());
        fs.apply_coarse_invalidation(file, "change feed backpressure limit exceeded");
        return Ok(last_event_id);
    }
    let last_event_id =
        cursor_advanced_to.or_else(|| records.last().map(|record| record.event_id.clone()));
    for record in records {
        fs.apply_namespace_change(file, record, source)?;
    }
    Ok(last_event_id)
}

#[cfg(test)]
mod tests {
    use super::{feed_error_requires_data_reconnect, is_feed_read_timeout};
    use crate::error::Error;

    #[test]
    fn feed_read_timeout_does_not_stale_data_client() {
        let timeout = Error::new(libc::ETIMEDOUT, "9P response timeout after 5.000s");
        assert!(is_feed_read_timeout(&timeout));
        assert!(!feed_error_requires_data_reconnect(&timeout));

        let reset = Error::new(libc::ECONNRESET, "connection reset by peer");
        assert!(!is_feed_read_timeout(&reset));
        assert!(feed_error_requires_data_reconnect(&reset));
    }
}
