use super::{
    feed_poll_path, parse_namespace_change_record, parse_namespace_path, select_feed_records,
    FeedEvent, FeedEventBus, FeedState,
};
use crate::{Client, ClientSlot, Error, NamespaceCache, Result, StaleReason, OREAD};
use r9p::fid::Fid;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct FeedWorkerConfig {
    pub path: String,
    pub stream_path: Option<String>,
    pub cursor_template: Option<String>,
    pub cache: Option<NamespaceCache>,
    pub event_bus: Option<FeedEventBus>,
    pub poll_interval: Duration,
    pub lookup_timeout: Duration,
    pub read_timeout: Duration,
    pub control_timeout: Duration,
    pub backpressure_limit: usize,
}

pub struct FeedWorkerHandle {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl FeedWorkerHandle {
    pub fn stop_and_join(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for FeedWorkerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub fn start_feed_worker(
    client: ClientSlot,
    config: FeedWorkerConfig,
    state: FeedState,
) -> Result<FeedWorkerHandle> {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let handle = thread::Builder::new()
        .name("r9p-session-feed".to_string())
        .spawn(move || feed_loop(client, config, state, thread_stop))
        .map_err(|error| Error::io("spawn namespace feed consumer", error))?;
    Ok(FeedWorkerHandle {
        stop,
        handle: Some(handle),
    })
}

fn feed_loop(
    client: ClientSlot,
    config: FeedWorkerConfig,
    state: FeedState,
    stop: Arc<AtomicBool>,
) {
    state.set_connecting();
    let mut since_event_id = None;
    while !stop.load(Ordering::SeqCst) {
        let Ok(client) = client.snapshot() else {
            state.set_degraded("9P client slot unavailable");
            sleep_interruptible(config.poll_interval, &stop);
            continue;
        };
        if let Some(stream_path) = config.stream_path.as_deref() {
            match consume_stream_until_error(&client, &config, stream_path, &state, &stop) {
                Ok(next_event_id) => {
                    if next_event_id.is_some() {
                        since_event_id = next_event_id;
                    }
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                }
                Err(error) => {
                    state.set_degraded(format!("stream feed degraded: {}", error.message()));
                }
            }
        }
        let poll_path = feed_poll_path(
            &config.path,
            since_event_id.as_deref(),
            config.cursor_template.as_deref(),
        );
        match consume_once(
            &client,
            &config,
            &poll_path,
            since_event_id.as_deref(),
            &state,
        ) {
            Ok(next_event_id) => {
                if next_event_id.is_some() {
                    since_event_id = next_event_id;
                }
            }
            Err(error) if error.errno == libc::ETIMEDOUT => {
                state.set_connected("poll", None, None);
            }
            Err(error) => {
                state.set_degraded(error.message().to_string());
            }
        }
        sleep_interruptible(config.poll_interval, &stop);
    }
}

fn consume_once(
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
        state.set_connected("poll", None, None);
        return Ok(None);
    }

    process_feed_data(&data, FeedReadMode::Poll { since_event_id }, config, state)
}

fn consume_stream_until_error(
    client: &Client,
    config: &FeedWorkerConfig,
    path: &str,
    state: &FeedState,
    stop: &AtomicBool,
) -> Result<Option<String>> {
    let fid = open_feed(client, path, config.lookup_timeout)?;
    let mut last_event_id = None;
    while !stop.load(Ordering::SeqCst) {
        match client.read_timeout(fid, 0, 64 * 1024, config.read_timeout) {
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
                let _ = client.clunk_timeout(fid, config.control_timeout);
                return Err(error);
            }
        }
    }
    let _ = client.clunk_timeout(fid, config.control_timeout);
    Ok(last_event_id)
}

#[derive(Clone, Copy)]
enum FeedReadMode<'a> {
    Stream,
    Poll { since_event_id: Option<&'a str> },
}

impl FeedReadMode<'_> {
    fn source(self) -> &'static str {
        match self {
            FeedReadMode::Stream => "stream",
            FeedReadMode::Poll { .. } => "poll",
        }
    }
}

fn process_feed_data(
    data: &[u8],
    mode: FeedReadMode<'_>,
    config: &FeedWorkerConfig,
    state: &FeedState,
) -> Result<Option<String>> {
    let text = String::from_utf8_lossy(data);
    let records = text
        .lines()
        .filter_map(parse_namespace_change_record)
        .collect::<Vec<_>>();
    let (records, cursor_advanced_to) = match mode {
        FeedReadMode::Stream => (records, None),
        FeedReadMode::Poll { since_event_id } => {
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

fn publish_feed_event(config: &FeedWorkerConfig, event: FeedEvent, state: &FeedState) {
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
