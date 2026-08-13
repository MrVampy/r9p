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
