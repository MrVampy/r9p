//! Small local status sink for long-running mounts.

use crate::{
    error::{Error, Result},
    fuse::read_cache::CacheSnapshot,
};
use std::{
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const CACHE_STATUS_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub(super) struct MountStatus {
    state: Arc<Mutex<State>>,
}

#[derive(Clone)]
struct State {
    path: Option<PathBuf>,
    namespace_source: String,
    active_endpoint: String,
    endpoint_candidates: Vec<String>,
    change_feed: &'static str,
    source: Option<&'static str>,
    last_event_id: Option<String>,
    last_error: Option<String>,
    read_cache: Option<CacheSnapshot>,
    last_cache_publish: Option<Instant>,
}

impl MountStatus {
    pub(super) fn new(
        path: Option<PathBuf>,
        namespace_source: String,
        active_endpoint: String,
        endpoint_candidates: Vec<String>,
    ) -> Self {
        let status = Self {
            state: Arc::new(Mutex::new(State {
                path,
                namespace_source,
                active_endpoint,
                endpoint_candidates,
                change_feed: "disabled",
                source: None,
                last_event_id: None,
                last_error: None,
                read_cache: None,
                last_cache_publish: None,
            })),
        };
        status.publish();
        status
    }

    pub(super) fn set_transport(&self, active_endpoint: String) {
        let snapshot = {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            state.active_endpoint = active_endpoint;
            state.clone()
        };
        let _ = write_status(snapshot);
    }

    pub(super) fn set_change_feed(
        &self,
        change_feed: &'static str,
        source: Option<&'static str>,
        last_event_id: Option<String>,
        last_error: Option<String>,
    ) {
        let snapshot = {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            state.change_feed = change_feed;
            state.source = source;
            if last_event_id.is_some() {
                state.last_event_id = last_event_id;
            }
            state.last_error = last_error;
            state.clone()
        };
        let _ = write_status(snapshot);
    }

    pub(super) fn set_read_cache(&self, read_cache: CacheSnapshot) {
        let snapshot = {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            state.read_cache = Some(read_cache);
            let now = Instant::now();
            if state
                .last_cache_publish
                .is_some_and(|last| now.duration_since(last) < CACHE_STATUS_INTERVAL)
            {
                return;
            }
            state.last_cache_publish = Some(now);
            state.clone()
        };
        let _ = write_status(snapshot);
    }

    fn publish(&self) {
        let snapshot = {
            let Ok(state) = self.state.lock() else {
                return;
            };
            state.clone()
        };
        let _ = write_status(snapshot);
    }
}

fn write_status(state: State) -> Result<()> {
    let Some(path) = state.path.as_ref() else {
        return Ok(());
    };
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|error| Error::io(format!("open status {}", path.display()), error))?;
    writeln!(file, "{}", status_json(&state))
        .map_err(|error| Error::io(format!("write status {}", path.display()), error))
}

fn status_json(state: &State) -> String {
    format!(
        "{{\"namespace_source\":\"{}\",\"active_endpoint\":\"{}\",\"endpoint_candidates\":{},\"change_feed\":\"{}\",\"source\":{},\"last_event_id\":{},\"last_error\":{},\"persistent_read_cache\":{}}}",
        escape_json(&state.namespace_source),
        escape_json(&state.active_endpoint),
        string_array_json(&state.endpoint_candidates),
        state.change_feed,
        optional_static_json(state.source),
        optional_json(&state.last_event_id),
        optional_json(&state.last_error),
        read_cache_json(state.read_cache)
    )
}

fn read_cache_json(snapshot: Option<CacheSnapshot>) -> String {
    let Some(snapshot) = snapshot else {
        return "{\"state\":\"disabled\"}".to_string();
    };
    format!(
        "{{\"state\":\"ready\",\"chunk_bytes\":{},\"max_bytes\":{},\"current_bytes\":{},\"hit_chunks\":{},\"miss_chunks\":{},\"hit_bytes\":{},\"fetched_bytes\":{},\"evictions\":{},\"read_errors\":{},\"write_errors\":{}}}",
        snapshot.chunk_bytes,
        snapshot.max_bytes,
        snapshot.current_bytes,
        snapshot.hit_chunks,
        snapshot.miss_chunks,
        snapshot.hit_bytes,
        snapshot.fetched_bytes,
        snapshot.evictions,
        snapshot.read_errors,
        snapshot.write_errors
    )
}

fn string_array_json(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| format!("\"{}\"", escape_json(value)))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn optional_static_json(value: Option<&str>) -> String {
    match value {
        Some(value) => format!("\"{}\"", escape_json(value)),
        None => "null".to_string(),
    }
}

fn optional_json(value: &Option<String>) -> String {
    match value {
        Some(value) => format!("\"{}\"", escape_json(value)),
        None => "null".to_string(),
    }
}

fn escape_json(value: &str) -> String {
    let mut out = String::new();
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            character if character.is_control() => {
                out.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => out.push(character),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{status_json, CacheSnapshot, State};

    #[test]
    fn status_json_reports_feed_state() {
        let json = status_json(&State {
            path: None,
            namespace_source: "/sources/newsgroups/browse".to_string(),
            active_endpoint: "nucbox.mesh:9564".to_string(),
            endpoint_candidates: vec!["m7.mesh:9564".to_string(), "nucbox.mesh:9564".to_string()],
            change_feed: "degraded",
            source: Some("stream"),
            last_event_id: Some("event-1".to_string()),
            last_error: Some("feed missing".to_string()),
            read_cache: Some(CacheSnapshot {
                chunk_bytes: 4 * 1024 * 1024,
                max_bytes: 1024 * 1024 * 1024,
                current_bytes: 8 * 1024 * 1024,
                hit_chunks: 3,
                miss_chunks: 2,
                hit_bytes: 4096,
                fetched_bytes: 8192,
                evictions: 1,
                read_errors: 0,
                write_errors: 0,
            }),
            last_cache_publish: None,
        });
        assert!(json.contains("\"namespace_source\":\"/sources/newsgroups/browse\""));
        assert!(json.contains("\"active_endpoint\":\"nucbox.mesh:9564\""));
        assert!(json.contains("\"endpoint_candidates\":[\"m7.mesh:9564\",\"nucbox.mesh:9564\"]"));
        assert!(json.contains("\"change_feed\":\"degraded\""));
        assert!(json.contains("\"source\":\"stream\""));
        assert!(json.contains("\"last_event_id\":\"event-1\""));
        assert!(json.contains("\"last_error\":\"feed missing\""));
        assert!(json.contains("\"persistent_read_cache\":{\"state\":\"ready\""));
        assert!(json.contains("\"hit_chunks\":3"));
    }
}
