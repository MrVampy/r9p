//! Runtime namespace-change feed consumer.
//!
//! The mount consumes generic path-change records only. Vault-specific domain
//! events are projected into this shape by the runtime before they reach this
//! Rust mechanism.

use super::{
    invalidation::{notify_kernel_invalidations, KernelInvalidation},
    util::is_transport_error,
    R9pFuse,
};
use crate::{
    error::{Error, Result},
    p9::{Client, OREAD},
};
use r9p::fid::Fid;
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
        self.clunk_stale_bindings(invalidation.stale_bindings);
        self.record_mount_diagnostic("change_feed_coarse_invalidation", 0, reason);
    }
}

fn change_feed_loop(fs: &mut R9pFuse, file: &mut File, path: String, stop: Arc<AtomicBool>) {
    fs.status.set_change_feed("connecting", None, None);
    while !stop.load(Ordering::SeqCst) {
        match consume_feed_until_error(fs, file, &path, &stop) {
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
                if is_transport_error(&error) {
                    let _ = fs.reconnect();
                }
                sleep_interruptible(fs.config.change_feed_poll_interval, &stop);
            }
        }
    }
}

fn consume_feed_until_error(
    fs: &mut R9pFuse,
    file: &mut File,
    path: &str,
    stop: &AtomicBool,
) -> Result<()> {
    fs.status.set_change_feed("connecting", None, None);
    let mut since_event_id = None;
    while !stop.load(Ordering::SeqCst) {
        let poll_path = feed_poll_path(path, since_event_id.as_deref());
        let client = fs.client_snapshot()?;
        let fid = open_feed(&client, &poll_path, fs.lookup_timeout())?;
        match client.read_full_timeout(fid, 0, 64 * 1024, fs.control_timeout()) {
            Ok(data) if data.is_empty() => {
                let _ = client.clunk_timeout(fid, fs.control_timeout());
                fs.status.set_change_feed("connected", None, None);
            }
            Ok(data) => {
                let _ = client.clunk_timeout(fid, fs.control_timeout());
                if let Some(event_id) = apply_feed_chunk(fs, file, &data)? {
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
                return Err(error);
            }
        }
        sleep_interruptible(fs.config.change_feed_poll_interval, stop);
    }
    Ok(())
}

fn open_feed(client: &Client, path: &str, timeout: Duration) -> Result<Fid> {
    let segments = parse_namespace_path(path)?;
    let fid = client.walk_timeout(client.root_fid(), &segments, timeout)?;
    if let Err(error) = client.open_timeout(fid, OREAD, timeout) {
        let _ = client.clunk_timeout(fid, timeout);
        return Err(error);
    }
    Ok(fid)
}

fn apply_feed_chunk(fs: &mut R9pFuse, file: &mut File, data: &[u8]) -> Result<Option<String>> {
    let text = String::from_utf8_lossy(data);
    let records = text
        .lines()
        .filter_map(parse_namespace_change_record)
        .collect::<Vec<_>>();
    if records.len() > fs.config.change_feed_backpressure_limit {
        let last_event_id = records.last().map(|record| record.event_id.clone());
        fs.apply_coarse_invalidation(file, "change feed backpressure limit exceeded");
        return Ok(last_event_id);
    }
    let last_event_id = records.last().map(|record| record.event_id.clone());
    for record in records {
        fs.apply_namespace_change(file, record)?;
    }
    Ok(last_event_id)
}

fn feed_poll_path(base_path: &str, since_event_id: Option<&str>) -> String {
    let Some(event_id) = since_event_id else {
        return base_path.to_string();
    };
    let trimmed = base_path.trim_end_matches('/');
    let root = trimmed
        .strip_suffix("/stream")
        .or_else(|| trimmed.strip_suffix("/recent"))
        .or_else(|| trimmed.rsplit_once("/since/").map(|(root, _)| root))
        .unwrap_or(trimmed);
    format!("{root}/since/{event_id}")
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct NamespaceChange {
    scope: String,
    path: String,
    change_kind: String,
    generation: u64,
    event_id: String,
    old_path: Option<String>,
}

fn parse_namespace_change_record(line: &str) -> Option<NamespaceChange> {
    let fields = line.split('\t').collect::<Vec<_>>();
    parse_key_value_record(&fields).or_else(|| parse_positional_record(&fields))
}

fn parse_key_value_record(fields: &[&str]) -> Option<NamespaceChange> {
    let mut scope = None;
    let mut path = None;
    let mut change_kind = None;
    let mut generation = None;
    let mut event_id = None;
    let mut old_path = None;
    for field in fields {
        let Some((key, value)) = field.split_once('=') else {
            continue;
        };
        match key {
            "scope" => scope = Some(value.to_string()),
            "path" => path = Some(value.to_string()),
            "change_kind" | "kind" => change_kind = Some(value.to_string()),
            "generation" => generation = value.parse::<u64>().ok(),
            "event_id" => event_id = Some(value.to_string()),
            "old_path" | "from" => old_path = Some(value.to_string()),
            _ => {}
        }
    }
    Some(NamespaceChange {
        scope: scope?,
        path: path?,
        change_kind: change_kind?,
        generation: generation?,
        event_id: event_id?,
        old_path,
    })
}

fn parse_positional_record(fields: &[&str]) -> Option<NamespaceChange> {
    match fields {
        ["namespace_change", event_id, generation, scope, change_kind, path] => {
            Some(NamespaceChange {
                scope: (*scope).to_string(),
                path: (*path).to_string(),
                change_kind: (*change_kind).to_string(),
                generation: generation.parse().ok()?,
                event_id: (*event_id).to_string(),
                old_path: None,
            })
        }
        ["namespace_change", event_id, generation, scope, "renamed", old_path, path] => {
            Some(NamespaceChange {
                scope: (*scope).to_string(),
                path: (*path).to_string(),
                change_kind: "renamed".to_string(),
                generation: generation.parse().ok()?,
                event_id: (*event_id).to_string(),
                old_path: Some((*old_path).to_string()),
            })
        }
        _ => None,
    }
}

fn parse_namespace_path(path: &str) -> Result<Vec<Vec<u8>>> {
    if !path.starts_with('/') {
        return Err(Error::new(
            libc::EINVAL,
            format!("namespace change path must be absolute: {path}"),
        ));
    }
    Ok(path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(|segment| segment.as_bytes().to_vec())
        .collect())
}

fn scope_matches(configured_scope: Option<&str>, event_scope: &str) -> bool {
    event_scope == "shared"
        || configured_scope
            .map(|scope| scope == event_scope)
            .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::{
        feed_poll_path, parse_namespace_change_record, parse_namespace_path, scope_matches,
    };

    #[test]
    fn parses_key_value_namespace_change_record() {
        let record = parse_namespace_change_record(
            "namespace_change\tevent_id=e1\tgeneration=42\tscope=shared\tchange_kind=created\tpath=/runtime/x",
        )
        .expect("record should parse");

        assert_eq!(record.event_id, "e1");
        assert_eq!(record.generation, 42);
        assert_eq!(record.scope, "shared");
        assert_eq!(record.change_kind, "created");
        assert_eq!(record.path, "/runtime/x");
    }

    #[test]
    fn parses_positional_rename_record() {
        let record = parse_namespace_change_record(
            "namespace_change\te2\t43\tsession:abc\trenamed\t/runtime/old\t/runtime/new",
        )
        .expect("record should parse");

        assert_eq!(record.old_path.as_deref(), Some("/runtime/old"));
        assert_eq!(record.path, "/runtime/new");
    }

    #[test]
    fn namespace_paths_are_absolute() {
        assert_eq!(
            parse_namespace_path("/runtime/status").expect("path should parse"),
            vec![b"runtime".to_vec(), b"status".to_vec()]
        );
        assert!(parse_namespace_path("runtime/status").is_err());
    }

    #[test]
    fn change_feed_scope_matches_shared_or_configured_scope() {
        assert!(scope_matches(Some("session:a"), "shared"));
        assert!(scope_matches(Some("session:a"), "session:a"));
        assert!(!scope_matches(Some("session:a"), "session:b"));
        assert!(scope_matches(None, "session:b"));
    }

    #[test]
    fn feed_poll_path_advances_with_since_cursor() {
        assert_eq!(
            feed_poll_path("/runtime/events/namespace/stream", Some("event-7")),
            "/runtime/events/namespace/since/event-7"
        );
        assert_eq!(
            feed_poll_path("/runtime/events/namespace/since/event-6", Some("event-7")),
            "/runtime/events/namespace/since/event-7"
        );
        assert_eq!(
            feed_poll_path("/runtime/events/namespace/recent", Some("event-7")),
            "/runtime/events/namespace/since/event-7"
        );
        assert_eq!(
            feed_poll_path("/runtime/events/namespace/stream", None),
            "/runtime/events/namespace/stream"
        );
    }
}
