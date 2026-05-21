use crate::error::{Error, Result};
use std::{
    collections::VecDeque,
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
};

pub const DEFAULT_DIAGNOSTICS_CAPACITY: usize = 256;

#[derive(Clone)]
pub(crate) struct Diagnostics {
    state: Arc<Mutex<State>>,
}

struct State {
    next_seq: u64,
    capacity: usize,
    path: Option<PathBuf>,
    entries: VecDeque<Diagnostic>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Diagnostic {
    pub seq: u64,
    pub event: &'static str,
    pub opcode: u32,
    pub unique: u64,
    pub nodeid: u64,
    pub errno: i32,
    pub message: String,
}

impl Diagnostics {
    pub(crate) fn new(capacity: usize, path: Option<PathBuf>) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                next_seq: 1,
                capacity: capacity.max(1),
                path,
                entries: VecDeque::new(),
            })),
        }
    }

    pub(crate) fn record(
        &self,
        event: &'static str,
        opcode: u32,
        unique: u64,
        nodeid: u64,
        errno: i32,
        message: impl Into<String>,
    ) -> Result<()> {
        let diagnostic = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| Error::new(libc::EIO, "diagnostics lock poisoned"))?;
            let diagnostic = Diagnostic {
                seq: state.next_seq,
                event,
                opcode,
                unique,
                nodeid,
                errno,
                message: message.into(),
            };
            state.next_seq = state.next_seq.saturating_add(1);
            while state.entries.len() >= state.capacity {
                state.entries.pop_front();
            }
            state.entries.push_back(diagnostic.clone());
            if let Some(path) = state.path.clone() {
                append_json_line(path, &diagnostic)?;
            }
            diagnostic
        };
        let _ = diagnostic;
        Ok(())
    }
}

impl Default for Diagnostics {
    fn default() -> Self {
        Self::new(DEFAULT_DIAGNOSTICS_CAPACITY, None)
    }
}

fn append_json_line(path: PathBuf, diagnostic: &Diagnostic) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| Error::io(format!("open diagnostics {}", path.display()), error))?;
    writeln!(file, "{}", diagnostic_json(diagnostic))
        .map_err(|error| Error::io(format!("write diagnostics {}", path.display()), error))
}

fn diagnostic_json(diagnostic: &Diagnostic) -> String {
    format!(
        "{{\"event\":\"{}\",\"seq\":{},\"opcode\":{},\"unique\":{},\"nodeid\":{},\"errno\":{},\"message\":\"{}\"}}",
        escape_json(diagnostic.event),
        diagnostic.seq,
        diagnostic.opcode,
        diagnostic.unique,
        diagnostic.nodeid,
        diagnostic.errno,
        escape_json(&diagnostic.message)
    )
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
    use super::{diagnostic_json, Diagnostic};

    #[test]
    fn diagnostics_are_json_lines() {
        let line = diagnostic_json(&Diagnostic {
            seq: 7,
            event: "operation_error",
            opcode: 15,
            unique: 99,
            nodeid: 1,
            errno: libc::ETIMEDOUT,
            message: "timed out\nwhile reading \"x\"".to_string(),
        });
        assert!(line.contains("\"event\":\"operation_error\""));
        assert!(line.contains("\"seq\":7"));
        assert!(line.contains("timed out\\nwhile reading \\\"x\\\""));
    }
}
