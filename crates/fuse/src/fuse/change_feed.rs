//! Runtime namespace-change feed consumer.
//!
//! The mount consumes generic path-change records only. Application-specific
//! domain events are projected into this shape before they reach this Rust
//! mechanism.

use super::{
    invalidation::{notify_kernel_invalidations, KernelInvalidation},
    R9pFuse,
};
use crate::error::{Error, Result};
use session::feed::{
    parse_namespace_path, scope_matches, start_feed_worker, FeedEvent, FeedEventBus,
    FeedEventReceiver, FeedReceiverWake, FeedState, FeedWorkerConfig, FeedWorkerHandle,
    NamespaceChange,
};
use std::{
    fs::File,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
};

pub(super) const DEFAULT_CHANGE_FEED_BACKPRESSURE_LIMIT: usize = 4096;

#[derive(Debug, Eq, PartialEq)]
enum MountedPath {
    Outside,
    Root,
    Relative(Vec<Vec<u8>>),
}

#[derive(Debug, Eq, PartialEq)]
enum MountedChange {
    Ignore,
    Root,
    Created(Vec<Vec<u8>>),
    Removed(Vec<Vec<u8>>),
    Modified(Vec<Vec<u8>>),
    Renamed {
        old: Vec<Vec<u8>>,
        new: Vec<Vec<u8>>,
    },
}

pub(super) struct ChangeFeedHandle {
    stop: Arc<AtomicBool>,
    receiver_wake: Option<FeedReceiverWake>,
    feed_worker: Option<FeedWorkerHandle>,
    handle: Option<JoinHandle<()>>,
}

impl ChangeFeedHandle {
    pub(super) fn stop_and_join(mut self) {
        self.signal_stop();
        if let Some(feed) = self.feed_worker.take() {
            feed.stop_and_join();
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }

    fn signal_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(wake) = &self.receiver_wake {
            wake.notify();
        }
    }
}

impl Drop for ChangeFeedHandle {
    fn drop(&mut self) {
        self.signal_stop();
        if let Some(feed) = self.feed_worker.take() {
            feed.stop_and_join();
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
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
        let bus = FeedEventBus::new(self.config.change_feed_backpressure_limit);
        let receiver = bus.subscribe();
        let receiver_wake = receiver.wake_handle();
        let feed_session = self.client.clone();
        let feed_worker = start_feed_worker(
            feed_session.clone(),
            FeedWorkerConfig {
                path,
                stream_path,
                cursor_template: self.config.change_feed_cursor_template.clone(),
                cache: None,
                event_bus: Some(bus),
                wake: None,
                reconnect_delay: self.config.change_feed_reconnect_delay,
                lookup_timeout: self.lookup_timeout(),
                read_timeout: self.config.change_feed_read_timeout,
                control_timeout: self.control_timeout(),
                backpressure_limit: self.config.change_feed_backpressure_limit,
            },
            FeedState::new(),
        )?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let mut fs = self.clone();
        let handle = match thread::Builder::new()
            .name("r9p-fuse-change-feed".to_string())
            .spawn(move || session_feed_event_loop(&mut fs, &mut file, receiver, thread_stop))
        {
            Ok(handle) => handle,
            Err(error) => {
                feed_worker.stop_and_join();
                return Err(Error::io("spawn namespace change-feed consumer", error));
            }
        };
        Ok(Some(ChangeFeedHandle {
            stop,
            receiver_wake: Some(receiver_wake),
            feed_worker: Some(feed_worker),
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
            feed_worker: None,
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
        let mounted = mounted_change(&self.source_path, &change)?;
        if mounted == MountedChange::Root {
            self.apply_coarse_invalidation(file, "change affected mounted subtree root");
            self.status
                .set_change_feed("connected", Some(source), Some(change.event_id), None);
            return Ok(());
        }
        let invalidation = match mounted {
            MountedChange::Ignore => None,
            MountedChange::Created(path) | MountedChange::Modified(path) => {
                let mut nodes = self.nodes()?;
                Some(KernelInvalidation::path(
                    nodes.mark_path_stale(&path),
                    nodes
                        .mark_parent_directory_cache_stale(&path)
                        .into_iter()
                        .collect(),
                ))
            }
            MountedChange::Removed(path) => {
                let mut nodes = self.nodes()?;
                Some(KernelInvalidation::path(
                    nodes.mark_path_prefix_stale(&path),
                    nodes
                        .mark_parent_directory_cache_stale(&path)
                        .into_iter()
                        .collect(),
                ))
            }
            MountedChange::Renamed { old, new } => {
                let mut nodes = self.nodes()?;
                let mut stale = nodes.mark_path_prefix_stale(&old);
                stale.extend(nodes.mark_path_prefix_stale(&new));
                let mut parent_entries = nodes
                    .mark_parent_directory_cache_stale(&old)
                    .into_iter()
                    .collect::<Vec<_>>();
                parent_entries.extend(nodes.mark_parent_directory_cache_stale(&new));
                Some(KernelInvalidation::path(stale, parent_entries))
            }
            MountedChange::Root => unreachable!(),
        };
        if let Some(invalidation) = invalidation {
            notify_kernel_invalidations(file, &invalidation);
            self.clunk_stale_bindings(invalidation.stale_bindings);
        }
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

fn mounted_path(source_path: &[Vec<u8>], path: &str) -> Result<MountedPath> {
    let path = parse_namespace_path(path)?;
    if source_path.is_empty() {
        return if path.is_empty() {
            Ok(MountedPath::Root)
        } else {
            Ok(MountedPath::Relative(path))
        };
    }
    if path.starts_with(source_path) {
        let relative = path[source_path.len()..].to_vec();
        return if relative.is_empty() {
            Ok(MountedPath::Root)
        } else {
            Ok(MountedPath::Relative(relative))
        };
    }
    if source_path.starts_with(&path) {
        return Ok(MountedPath::Root);
    }
    Ok(MountedPath::Outside)
}

fn mounted_change(source_path: &[Vec<u8>], change: &NamespaceChange) -> Result<MountedChange> {
    if !matches!(
        change.change_kind.as_str(),
        "created" | "removed" | "modified" | "renamed"
    ) {
        return Err(Error::new(
            libc::EINVAL,
            format!("unknown namespace change kind {}", change.change_kind),
        ));
    }
    let path = mounted_path(source_path, &change.path)?;
    let old_path = change
        .old_path
        .as_deref()
        .map(|path| mounted_path(source_path, path))
        .transpose()?
        .unwrap_or(MountedPath::Outside);
    if path == MountedPath::Root || old_path == MountedPath::Root {
        return Ok(MountedChange::Root);
    }
    match (change.change_kind.as_str(), old_path, path) {
        ("created", _, MountedPath::Relative(path)) => Ok(MountedChange::Created(path)),
        ("created", _, MountedPath::Outside) => Ok(MountedChange::Ignore),
        ("removed", _, MountedPath::Relative(path)) => Ok(MountedChange::Removed(path)),
        ("removed", _, MountedPath::Outside) => Ok(MountedChange::Ignore),
        ("modified", _, MountedPath::Relative(path)) => Ok(MountedChange::Modified(path)),
        ("modified", _, MountedPath::Outside) => Ok(MountedChange::Ignore),
        ("renamed", MountedPath::Relative(old), MountedPath::Relative(new)) => {
            Ok(MountedChange::Renamed { old, new })
        }
        ("renamed", MountedPath::Relative(old), MountedPath::Outside) => {
            Ok(MountedChange::Removed(old))
        }
        ("renamed", MountedPath::Outside, MountedPath::Relative(new)) => {
            Ok(MountedChange::Created(new))
        }
        ("renamed", MountedPath::Outside, MountedPath::Outside) => Ok(MountedChange::Ignore),
        (_, _, MountedPath::Root) | ("renamed", MountedPath::Root, _) => unreachable!(),
        _ => unreachable!(),
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
            Ok(Some(FeedEvent::Change { change, source, .. })) => {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> Vec<Vec<u8>> {
        parse_namespace_path("/sources/newsgroups/downloads/files").expect("source")
    }

    fn change(kind: &str, path: &str, old_path: Option<&str>) -> NamespaceChange {
        NamespaceChange {
            scope: "shared".to_string(),
            path: path.to_string(),
            change_kind: kind.to_string(),
            generation: 1,
            event_id: "change-1".to_string(),
            old_path: old_path.map(str::to_string),
        }
    }

    #[test]
    fn subtree_change_paths_are_projected_relative_to_the_mount_root() {
        assert_eq!(
            mounted_path(
                &source(),
                "/sources/newsgroups/downloads/files/acq-example/video.mp4"
            )
            .expect("path"),
            MountedPath::Relative(vec![b"acq-example".to_vec(), b"video.mp4".to_vec()])
        );
        assert_eq!(
            mounted_path(&source(), "/sources/newsgroups/downloads/files").expect("root"),
            MountedPath::Root
        );
    }

    #[test]
    fn ancestor_changes_invalidate_the_mount_and_siblings_are_ignored() {
        assert_eq!(
            mounted_path(&source(), "/sources/newsgroups/downloads").expect("ancestor"),
            MountedPath::Root
        );
        assert_eq!(
            mounted_path(&source(), "/sources/newsgroups/status").expect("outside"),
            MountedPath::Outside
        );
    }

    #[test]
    fn root_mounts_keep_absolute_namespace_paths_relative_to_their_root() {
        assert_eq!(
            mounted_path(&[], "/sources/newsgroups/status").expect("path"),
            MountedPath::Relative(vec![
                b"sources".to_vec(),
                b"newsgroups".to_vec(),
                b"status".to_vec()
            ])
        );
    }

    #[test]
    fn renames_across_the_source_boundary_become_create_or_remove() {
        assert_eq!(
            mounted_change(
                &source(),
                &change(
                    "renamed",
                    "/sources/newsgroups/downloads/files/acq/video.mp4",
                    Some("/sources/newsgroups/staging/video.mp4")
                )
            )
            .expect("rename into mount"),
            MountedChange::Created(vec![b"acq".to_vec(), b"video.mp4".to_vec()])
        );
        assert_eq!(
            mounted_change(
                &source(),
                &change(
                    "renamed",
                    "/sources/newsgroups/staging/video.mp4",
                    Some("/sources/newsgroups/downloads/files/acq/video.mp4")
                )
            )
            .expect("rename out of mount"),
            MountedChange::Removed(vec![b"acq".to_vec(), b"video.mp4".to_vec()])
        );
    }
}
