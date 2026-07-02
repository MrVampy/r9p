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
        feed_poll_path, parse_namespace_change_record, parse_namespace_path, scope_matches,
        select_feed_records, NamespaceChange,
    },
    Client, OREAD,
};
use std::{
    fs::File,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

pub(super) const DEFAULT_CHANGE_FEED_BACKPRESSURE_LIMIT: usize = 4096;

pub(super) struct ChangeFeedHandle {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl ChangeFeedHandle {
    pub(super) fn stop_and_join(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for ChangeFeedHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl R9pFuse {
    pub(super) fn start_change_feed(&self, file: &File) -> Result<Option<ChangeFeedHandle>> {
        let Some(path) = self.config.change_feed_path.clone() else {
            self.status.set_change_feed("disabled", None, None);
            return Ok(None);
        };
        let mut file = file
            .try_clone()
            .map_err(|error| Error::io("clone /dev/fuse for change feed", error))?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let mut fs = self.clone();
        let handle = thread::Builder::new()
            .name("r9p-fuse-change-feed".to_string())
            .spawn(move || change_feed_loop(&mut fs, &mut file, path, thread_stop))
            .map_err(|error| Error::io("spawn namespace change-feed consumer", error))?;
        Ok(Some(ChangeFeedHandle {
            stop,
            handle: Some(handle),
        }))
    }

    fn apply_namespace_change(&mut self, file: &mut File, change: NamespaceChange) -> Result<()> {
        if !scope_matches(self.config.change_feed_scope.as_deref(), &change.scope) {
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
                    nodes.parent_entry(&path).into_iter().collect(),
                ),
                "removed" => KernelInvalidation::path(
                    nodes.mark_path_prefix_stale(&path),
                    nodes.parent_entry(&path).into_iter().collect(),
                ),
                "renamed" => {
                    let mut stale = old_path
                        .as_deref()
                        .map(|old| nodes.mark_path_prefix_stale(old))
                        .unwrap_or_default();
                    stale.extend(nodes.mark_path_prefix_stale(&path));
                    let mut parent_entries = Vec::new();
                    if let Some(old) = old_path.as_deref() {
                        parent_entries.extend(nodes.parent_entry(old));
                    }
                    parent_entries.extend(nodes.parent_entry(&path));
                    KernelInvalidation::path(stale, parent_entries)
                }
                "modified" => KernelInvalidation::path(
                    nodes.mark_path_stale(&path),
                    nodes.parent_entry(&path).into_iter().collect(),
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
            .set_change_feed("connected", Some(change.event_id), None);
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

fn change_feed_loop(fs: &mut R9pFuse, file: &mut File, path: String, stop: Arc<AtomicBool>) {
    fs.status.set_change_feed("connecting", None, None);
    let mut feed_client = None;
    let mut data_client_stale = false;
    while !stop.load(Ordering::SeqCst) {
        let client = match change_feed_client(fs, &mut feed_client) {
            Ok(client) => client,
            Err(error) => {
                data_client_stale = true;
                fs.status
                    .set_change_feed("degraded", None, Some(error.message().to_string()));
                fs.record_mount_diagnostic(
                    "change_feed_disconnected",
                    error.errno,
                    error.message(),
                );
                fs.apply_coarse_invalidation(file, "change feed degraded");
                sleep_interruptible(fs.config.change_feed_poll_interval, &stop);
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
                    fs.status
                        .set_change_feed("degraded", None, Some(error.message().to_string()));
                    fs.record_mount_diagnostic(
                        "change_feed_data_reconnect_failed",
                        error.errno,
                        error.message(),
                    );
                    fs.apply_coarse_invalidation(file, "change feed data reconnect failed");
                    sleep_interruptible(fs.config.change_feed_poll_interval, &stop);
                    continue;
                }
            }
        }
        match consume_feed_until_error(fs, file, &path, &stop, &client) {
            Ok(()) => {}
            Err(error) => {
                fs.status
                    .set_change_feed("degraded", None, Some(error.message().to_string()));
                fs.record_mount_diagnostic(
                    "change_feed_disconnected",
                    error.errno,
                    error.message(),
                );
                fs.apply_coarse_invalidation(file, "change feed degraded");
                if feed_error_requires_data_reconnect(&error) {
                    feed_client = None;
                    data_client_stale = true;
                }
                sleep_interruptible(fs.config.change_feed_poll_interval, &stop);
            }
        }
    }
}

fn change_feed_client(fs: &R9pFuse, slot: &mut Option<Client>) -> Result<Client> {
    if let Some(client) = slot {
        return Ok(client.clone());
    }
    let client = Client::connect_with_timeout(
        &fs.config.address,
        &fs.config.uname,
        &fs.config.aname,
        fs.config.msize,
        fs.config.connect_timeout,
    )?;
    *slot = Some(client.clone());
    Ok(client)
}

fn consume_feed_until_error(
    fs: &mut R9pFuse,
    file: &mut File,
    path: &str,
    stop: &AtomicBool,
    client: &Client,
) -> Result<()> {
    fs.status.set_change_feed("connecting", None, None);
    let mut since_event_id = None;
    while !stop.load(Ordering::SeqCst) {
        let poll_path = feed_poll_path(
            path,
            since_event_id.as_deref(),
            fs.config.change_feed_cursor_template.as_deref(),
        );
        let fid = match open_feed(client, &poll_path, fs.lookup_timeout()) {
            Ok(fid) => fid,
            Err(error) if is_feed_poll_timeout(&error) => {
                fs.status.set_change_feed("connected", None, None);
                sleep_interruptible(fs.config.change_feed_poll_interval, stop);
                continue;
            }
            Err(error) => return Err(error),
        };
        match client.read_timeout(fid, 0, 64 * 1024, fs.read_timeout()) {
            Ok(data) if data.is_empty() => {
                let _ = client.clunk_timeout(fid, fs.control_timeout());
                fs.status.set_change_feed("connected", None, None);
            }
            Ok(data) => {
                let _ = client.clunk_timeout(fid, fs.control_timeout());
                if let Some(event_id) = apply_feed_chunk(
                    fs,
                    file,
                    &data,
                    since_event_id.as_deref(),
                    fs.config.change_feed_cursor_template.is_some(),
                )? {
                    since_event_id = Some(event_id);
                }
                fs.status.set_change_feed("connected", None, None);
            }
            Err(error) if error.errno == libc::ETIMEDOUT => {
                let _ = client.clunk_timeout(fid, fs.control_timeout());
                fs.status.set_change_feed("connected", None, None);
            }
            Err(error) => {
                let _ = client.clunk_timeout(fid, fs.control_timeout());
                return Err(error.into());
            }
        }
        sleep_interruptible(fs.config.change_feed_poll_interval, stop);
    }
    Ok(())
}

fn is_feed_poll_timeout(error: &Error) -> bool {
    error.errno == libc::ETIMEDOUT
}

fn feed_error_requires_data_reconnect(error: &Error) -> bool {
    is_transport_error(error) && !is_feed_poll_timeout(error)
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

fn apply_feed_chunk(
    fs: &mut R9pFuse,
    file: &mut File,
    data: &[u8],
    since_event_id: Option<&str>,
    cursor_template_configured: bool,
) -> Result<Option<String>> {
    let text = String::from_utf8_lossy(data);
    let parsed_records = text
        .lines()
        .filter_map(parse_namespace_change_record)
        .collect::<Vec<_>>();
    let selected = select_feed_records(parsed_records, since_event_id, cursor_template_configured);
    if selected.cursor_missed {
        fs.apply_coarse_invalidation(file, "change feed cursor fell outside recent window");
    }
    let records = selected.records;
    if records.len() > fs.config.change_feed_backpressure_limit {
        let last_event_id = records.last().map(|record| record.event_id.clone());
        fs.apply_coarse_invalidation(file, "change feed backpressure limit exceeded");
        return Ok(last_event_id);
    }
    let last_event_id = selected
        .cursor_advanced_to
        .or_else(|| records.last().map(|record| record.event_id.clone()));
    for record in records {
        fs.apply_namespace_change(file, record)?;
    }
    Ok(last_event_id)
}

fn sleep_interruptible(duration: Duration, stop: &AtomicBool) {
    let step = Duration::from_millis(50);
    let mut slept = Duration::ZERO;
    while slept < duration && !stop.load(Ordering::SeqCst) {
        let remaining = duration.saturating_sub(slept);
        let current = remaining.min(step);
        thread::sleep(current);
        slept = slept.saturating_add(current);
    }
}

#[cfg(test)]
mod tests {
    use super::{feed_error_requires_data_reconnect, is_feed_poll_timeout};
    use crate::error::Error;

    #[test]
    fn feed_poll_timeout_does_not_stale_data_client() {
        let timeout = Error::new(libc::ETIMEDOUT, "9P response timeout after 5.000s");
        assert!(is_feed_poll_timeout(&timeout));
        assert!(!feed_error_requires_data_reconnect(&timeout));

        let reset = Error::new(libc::ECONNRESET, "connection reset by peer");
        assert!(!is_feed_poll_timeout(&reset));
        assert!(feed_error_requires_data_reconnect(&reset));
    }
}
