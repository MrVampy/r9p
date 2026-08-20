//! Small local status sink for long-running mounts.

use crate::error::{Error, Result};
use std::{
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
};

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
        "{{\"namespace_source\":\"{}\",\"active_endpoint\":\"{}\",\"endpoint_candidates\":{},\"change_feed\":\"{}\",\"source\":{},\"last_event_id\":{},\"last_error\":{}}}",
        escape_json(&state.namespace_source),
        escape_json(&state.active_endpoint),
        string_array_json(&state.endpoint_candidates),
        state.change_feed,
        optional_static_json(state.source),
        optional_json(&state.last_event_id),
        optional_json(&state.last_error)
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
    use super::{status_json, State};

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
        });
        assert!(json.contains("\"namespace_source\":\"/sources/newsgroups/browse\""));
        assert!(json.contains("\"active_endpoint\":\"nucbox.mesh:9564\""));
        assert!(json.contains("\"endpoint_candidates\":[\"m7.mesh:9564\",\"nucbox.mesh:9564\"]"));
        assert!(json.contains("\"change_feed\":\"degraded\""));
        assert!(json.contains("\"source\":\"stream\""));
        assert!(json.contains("\"last_event_id\":\"event-1\""));
        assert!(json.contains("\"last_error\":\"feed missing\""));
    }
}
