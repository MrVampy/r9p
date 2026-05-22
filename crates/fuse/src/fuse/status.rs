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
    change_feed: &'static str,
    last_event_id: Option<String>,
    last_error: Option<String>,
}

impl MountStatus {
    pub(super) fn new(path: Option<PathBuf>) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                path,
                change_feed: "disabled",
                last_event_id: None,
                last_error: None,
            })),
        }
    }

    pub(super) fn set_change_feed(
        &self,
        change_feed: &'static str,
        last_event_id: Option<String>,
        last_error: Option<String>,
    ) {
        let snapshot = {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            state.change_feed = change_feed;
            if last_event_id.is_some() {
                state.last_event_id = last_event_id;
            }
            state.last_error = last_error;
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
        "{{\"change_feed\":\"{}\",\"last_event_id\":{},\"last_error\":{}}}",
        state.change_feed,
        optional_json(&state.last_event_id),
        optional_json(&state.last_error)
    )
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
            change_feed: "degraded",
            last_event_id: Some("event-1".to_string()),
            last_error: Some("feed missing".to_string()),
        });
        assert!(json.contains("\"change_feed\":\"degraded\""));
        assert!(json.contains("\"last_event_id\":\"event-1\""));
        assert!(json.contains("\"last_error\":\"feed missing\""));
    }
}
