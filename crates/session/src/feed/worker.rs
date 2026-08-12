use super::{
    feed_catch_up_path, parse_namespace_change_record, parse_namespace_path, select_feed_records,
    FeedEvent, FeedEventBus, FeedState, FeedWake,
};
use crate::{
    Client, ClientSession, ConcurrentReadFid, Error, NamespaceCache, Result, StaleReason, OREAD,
};
use r9p::fid::Fid;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct FeedWorkerConfig {
    pub path: String,
    pub stream_path: String,
    pub cursor_template: Option<String>,
    pub cache: Option<NamespaceCache>,
    pub event_bus: Option<FeedEventBus>,
    pub wake: Option<FeedWake>,
    pub reconnect_delay: Duration,
    pub lookup_timeout: Duration,
    pub read_timeout: Duration,
    pub control_timeout: Duration,
    pub backpressure_limit: usize,
}

pub struct FeedWorkerHandle {
    stop: Arc<AtomicBool>,
    cancellation: Arc<FeedCancellation>,
    handle: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct FeedCancellation {
    stream: Mutex<Option<ConcurrentReadFid>>,
}

impl FeedWorkerHandle {
    pub fn stop_and_join(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.cancellation.cancel();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for FeedWorkerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.cancellation.cancel();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub fn start_feed_worker(
    client: ClientSession,
    config: FeedWorkerConfig,
    state: FeedState,
) -> Result<FeedWorkerHandle> {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let cancellation = Arc::new(FeedCancellation::default());
    let thread_cancellation = Arc::clone(&cancellation);
    let handle = thread::Builder::new()
        .name("r9p-session-feed".to_string())
        .spawn(move || feed_loop(client, config, state, thread_stop, thread_cancellation))
        .map_err(|error| Error::io("spawn namespace feed consumer", error))?;
    Ok(FeedWorkerHandle {
        stop,
        cancellation,
        handle: Some(handle),
    })
}

fn feed_loop(
    client: ClientSession,
    config: FeedWorkerConfig,
    state: FeedState,
    stop: Arc<AtomicBool>,
    cancellation: Arc<FeedCancellation>,
) {
    state.set_connecting();
    let mut since_event_id = None;
    while !stop.load(Ordering::SeqCst) {
        let attachment = match client.snapshot() {
            Ok(client) => client,
            Err(error) => {
                state.set_degraded(format!(
                    "9P client session unavailable: {}",
                    error.message()
                ));
                sleep_interruptible(config.reconnect_delay, &stop);
                continue;
            }
        };
        let stream = match attachment
            .open_concurrent_read_path_timeout(&config.stream_path, config.lookup_timeout)
        {
            Ok(stream) => stream,
            Err(error) => {
                publish_connection_loss(&config, &state, &error);
                if !stop.load(Ordering::SeqCst) {
                    reconnect_after_failure(&client, &attachment, &config, &state, &stop);
                }
                continue;
            }
        };
        if let Err(error) = cancellation.install(stream.clone(), &stop) {
            let _ = stream.cancel();
            if !stop.load(Ordering::SeqCst) {
                publish_connection_loss(&config, &state, &error);
            }
            break;
        }
        if let Some(wake) = &config.wake {
            wake.notify();
        }
        if let Some(event_id) = since_event_id.as_deref() {
            let catch_up_path = feed_catch_up_path(
                &config.path,
                Some(event_id),
                config.cursor_template.as_deref(),
            );
            match consume_catch_up(&attachment, &config, &catch_up_path, Some(event_id), &state) {
                Ok(next_event_id) => {
                    if next_event_id.is_some() {
                        since_event_id = next_event_id;
                    }
                }
                Err(error) if error.errno == libc::ETIMEDOUT => {}
                Err(error) => {
                    cancellation.clear();
                    let _ = stream.cancel();
                    publish_connection_loss(&config, &state, &error);
                    if !stop.load(Ordering::SeqCst) {
                        reconnect_after_failure(&client, &attachment, &config, &state, &stop);
                    }
                    continue;
                }
            }
        }
        match consume_stream_until_error(&stream, &config, &state, &stop) {
            Ok(next_event_id) => {
                cancellation.clear();
                if next_event_id.is_some() {
                    since_event_id = next_event_id;
                }
                if stop.load(Ordering::SeqCst) {
                    break;
                }
            }
            Err(error) => {
                cancellation.clear();
                since_event_id = state.snapshot().last_event_id.or(since_event_id);
                if !stop.load(Ordering::SeqCst) {
                    publish_connection_loss(&config, &state, &error);
                }
            }
        }
        if stop.load(Ordering::SeqCst) {
            break;
        }
        reconnect_after_failure(&client, &attachment, &config, &state, &stop);
    }
    if let Some(wake) = &config.wake {
        wake.close();
    }
}

fn consume_catch_up(
    client: &Client,
    config: &FeedWorkerConfig,
    path: &str,
    since_event_id: Option<&str>,
    state: &FeedState,
) -> Result<Option<String>> {
    let fid = open_feed(client, path, config.lookup_timeout)?;
    let read = client.read_timeout(fid, 0, 64 * 1024, config.read_timeout);
    let clunk = client.clunk_timeout(fid, config.control_timeout);
    let data = read?;
    clunk?;
    if data.is_empty() {
        state.set_connected("catch_up", None, None);
        return Ok(None);
    }

    process_feed_data(
        &data,
        FeedReadMode::CatchUp { since_event_id },
        config,
        state,
    )
}

fn consume_stream_until_error(
    stream: &ConcurrentReadFid,
    config: &FeedWorkerConfig,
    state: &FeedState,
    stop: &AtomicBool,
) -> Result<Option<String>> {
    let mut last_event_id = None;
    while !stop.load(Ordering::SeqCst) {
        match stream.read_timeout(0, 64 * 1024, config.read_timeout) {
            Ok(data) if data.is_empty() => {
                state.set_connected("stream", None, None);
            }
            Ok(data) => {
                if let Some(event_id) =
                    process_feed_data(&data, FeedReadMode::Stream, config, state)?
                {
                    last_event_id = Some(event_id);
                }
            }
            Err(error) if error.errno == libc::ETIMEDOUT => {
                state.set_connected("stream", None, None);
            }
            Err(error) => {
                let _ = stream.cancel();
                return Err(error);
            }
        }
    }
    let _ = stream.cancel();
    Ok(last_event_id)
}

impl FeedCancellation {
    fn install(&self, stream: ConcurrentReadFid, stop: &AtomicBool) -> Result<()> {
        if stop.load(Ordering::SeqCst) {
            return Err(Error::new(libc::ESHUTDOWN, "namespace feed stopped"));
        }
        let mut current = self
            .stream
            .lock()
            .map_err(|_| Error::new(libc::EIO, "namespace feed cancellation lock poisoned"))?;
        if stop.load(Ordering::SeqCst) {
            return Err(Error::new(libc::ESHUTDOWN, "namespace feed stopped"));
        }
        *current = Some(stream);
        Ok(())
    }

    fn clear(&self) {
        if let Ok(mut current) = self.stream.lock() {
            *current = None;
        }
    }

    fn cancel(&self) {
        let stream = self
            .stream
            .lock()
            .ok()
            .and_then(|mut current| current.take());
        if let Some(stream) = stream {
            let _ = stream.cancel();
        }
    }
}

fn publish_connection_loss(config: &FeedWorkerConfig, state: &FeedState, error: &Error) {
    publish_feed_event(
        config,
        FeedEvent::CoarseInvalidation {
            reason: "change feed connection lost".to_string(),
        },
        state,
    );
    state.set_degraded(format!("stream feed degraded: {}", error.message()));
}

fn reconnect_after_failure(
    client: &ClientSession,
    attachment: &Client,
    config: &FeedWorkerConfig,
    state: &FeedState,
    stop: &AtomicBool,
) {
    match client.reconnect_after(attachment) {
        Ok(_) => {}
        Err(error) => {
            state.set_degraded(format!(
                "stream feed reconnect degraded: {}",
                error.message()
            ));
            sleep_interruptible(config.reconnect_delay, stop);
        }
    }
}

#[derive(Clone, Copy)]
enum FeedReadMode<'a> {
    Stream,
    CatchUp { since_event_id: Option<&'a str> },
}

impl FeedReadMode<'_> {
    fn source(self) -> &'static str {
        match self {
            FeedReadMode::Stream => "stream",
            FeedReadMode::CatchUp { .. } => "catch_up",
        }
    }
}

fn process_feed_data(
    data: &[u8],
    mode: FeedReadMode<'_>,
    config: &FeedWorkerConfig,
    state: &FeedState,
) -> Result<Option<String>> {
    let records = match parse_feed_records(data) {
        Some(records) => records,
        None => {
            if let Some(cache) = &config.cache {
                cache.mark_all_stale(StaleReason::Explicit(
                    "change feed record malformed".to_string(),
                ));
            }
            publish_feed_event(
                config,
                FeedEvent::CoarseInvalidation {
                    reason: "change feed record malformed".to_string(),
                },
                state,
            );
            state.set_degraded("change feed record malformed");
            return Ok(None);
        }
    };
    let (records, cursor_advanced_to) = match mode {
        FeedReadMode::Stream => (records, None),
        FeedReadMode::CatchUp { since_event_id } => {
            let selected =
                select_feed_records(records, since_event_id, config.cursor_template.is_some());
            if selected.cursor_missed {
                if let Some(cache) = &config.cache {
                    cache.mark_all_stale(StaleReason::NamespaceChange);
                }
                publish_feed_event(
                    config,
                    FeedEvent::CoarseInvalidation {
                        reason: "change feed cursor fell outside recent window".to_string(),
                    },
                    state,
                );
                state.mark_fresh_instance();
                state.set_connected(mode.source(), selected.cursor_advanced_to.clone(), None);
                return Ok(selected.cursor_advanced_to);
            }
            (selected.records, selected.cursor_advanced_to)
        }
    };
    let backpressure_limit = if config.backpressure_limit == 0 {
        usize::MAX
    } else {
        config.backpressure_limit
    };
    if records.len() > backpressure_limit {
        if let Some(cache) = &config.cache {
            cache.mark_all_stale(StaleReason::Explicit(
                "change feed backpressure limit exceeded".to_string(),
            ));
        }
        publish_feed_event(
            config,
            FeedEvent::CoarseInvalidation {
                reason: "change feed backpressure limit exceeded".to_string(),
            },
            state,
        );
        state.set_degraded("change feed backpressure limit exceeded");
        return Ok(records.last().map(|record| record.event_id.clone()));
    }
    for record in &records {
        if let Some(cache) = &config.cache {
            cache.mark_namespace_change(&record.path, record.old_path.as_deref());
        }
        publish_feed_event(
            config,
            FeedEvent::Change {
                change: record.clone(),
                source: mode.source(),
            },
            state,
        );
        state.set_connected(
            mode.source(),
            Some(record.event_id.clone()),
            Some(record.generation),
        );
    }
    Ok(cursor_advanced_to.or_else(|| records.last().map(|record| record.event_id.clone())))
}

fn parse_feed_records(data: &[u8]) -> Option<Vec<super::NamespaceChange>> {
    std::str::from_utf8(data)
        .ok()?
        .lines()
        .filter(|line| !line.is_empty())
        .map(parse_namespace_change_record)
        .collect()
}

fn publish_feed_event(config: &FeedWorkerConfig, event: FeedEvent, state: &FeedState) {
    if let Some(wake) = &config.wake {
        wake.notify();
    }
    let Some(bus) = &config.event_bus else {
        return;
    };
    if !bus.publish(event) {
        if let Some(cache) = &config.cache {
            cache.mark_all_stale(StaleReason::Explicit(
                "change feed event subscriber backpressure".to_string(),
            ));
        }
        state.set_degraded("change feed event subscriber backpressure");
    }
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
    use super::*;
    use std::sync::mpsc::RecvTimeoutError;

    fn config(bus: FeedEventBus) -> FeedWorkerConfig {
        FeedWorkerConfig {
            path: "/changes/recent".to_string(),
            stream_path: "/changes/stream".to_string(),
            cursor_template: Some("/changes/after/{event_id}".to_string()),
            cache: None,
            event_bus: Some(bus),
            wake: None,
            reconnect_delay: Duration::from_secs(1),
            lookup_timeout: Duration::from_secs(1),
            read_timeout: Duration::from_secs(1),
            control_timeout: Duration::from_secs(1),
            backpressure_limit: 16,
        }
    }

    #[test]
    fn malformed_feed_record_fails_closed_to_coarse_invalidation() {
        let bus = FeedEventBus::new(16);
        let receiver = bus.subscribe();
        let state = FeedState::new();

        assert_eq!(
            process_feed_data(
                b"future_record\tevent-1\n",
                FeedReadMode::Stream,
                &config(bus),
                &state,
            )
            .expect("malformed record should degrade without ending the stream"),
            None
        );
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(1)),
            Ok(FeedEvent::CoarseInvalidation { .. })
        ));
        assert_eq!(state.snapshot().state, "degraded");
    }

    #[test]
    fn invalid_utf8_feed_record_fails_closed() {
        let bus = FeedEventBus::new(16);
        let receiver = bus.subscribe();
        let state = FeedState::new();

        process_feed_data(
            &[0xff, b'\n'],
            FeedReadMode::Stream,
            &config(bus.clone()),
            &state,
        )
        .expect("invalid UTF-8 should degrade without ending the stream");
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(1)),
            Ok(FeedEvent::CoarseInvalidation { .. })
        ));
        assert!(!matches!(
            receiver.recv_timeout(Duration::ZERO),
            Err(RecvTimeoutError::Disconnected)
        ));
    }
}
