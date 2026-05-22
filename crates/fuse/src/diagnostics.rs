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
    pub context: DiagnosticContext,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DiagnosticContext {
    pub path: Option<String>,
    pub fh: Option<u64>,
    pub offset: Option<u64>,
    pub size: Option<u64>,
    pub tag: Option<u16>,
}

pub(crate) struct DiagnosticRecord {
    pub event: &'static str,
    pub opcode: u32,
    pub unique: u64,
    pub nodeid: u64,
    pub errno: i32,
    pub message: String,
    pub context: DiagnosticContext,
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
        self.record_entry(DiagnosticRecord {
            event,
            opcode,
            unique,
            nodeid,
            errno,
            message: message.into(),
            context: DiagnosticContext::default(),
        })
    }

    pub(crate) fn record_entry(&self, record: DiagnosticRecord) -> Result<()> {
        let (path, entries) = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| Error::new(libc::EIO, "diagnostics lock poisoned"))?;
            let diagnostic = Diagnostic {
                seq: state.next_seq,
                event: record.event,
                opcode: record.opcode,
                unique: record.unique,
                nodeid: record.nodeid,
                errno: record.errno,
                message: record.message,
                context: record.context,
            };
            state.next_seq = state.next_seq.saturating_add(1);
            while state.entries.len() >= state.capacity {
                state.entries.pop_front();
            }
            state.entries.push_back(diagnostic.clone());
            (
                state.path.clone(),
                state.entries.iter().cloned().collect::<Vec<_>>(),
            )
        };
        if let Some(path) = path {
            write_json_lines(path, &entries)?;
        }
        Ok(())
    }
}

impl Default for Diagnostics {
    fn default() -> Self {
        Self::new(DEFAULT_DIAGNOSTICS_CAPACITY, None)
    }
}

fn write_json_lines(path: PathBuf, diagnostics: &[Diagnostic]) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)
        .map_err(|error| Error::io(format!("open diagnostics {}", path.display()), error))?;
    for diagnostic in diagnostics {
        writeln!(file, "{}", diagnostic_json(diagnostic))
            .map_err(|error| Error::io(format!("write diagnostics {}", path.display()), error))?;
    }
    Ok(())
}

fn diagnostic_json(diagnostic: &Diagnostic) -> String {
    let mut fields = format!(
        "{{\"event\":\"{}\",\"seq\":{},\"opcode\":{},\"unique\":{},\"nodeid\":{},\"errno\":{},\"message\":\"{}\"}}",
        escape_json(diagnostic.event),
        diagnostic.seq,
        diagnostic.opcode,
        diagnostic.unique,
        diagnostic.nodeid,
        diagnostic.errno,
        escape_json(&diagnostic.message)
    );
    fields.pop();
    if let Some(path) = &diagnostic.context.path {
        fields.push_str(&format!(",\"path\":\"{}\"", escape_json(path)));
    }
    if let Some(fh) = diagnostic.context.fh {
        fields.push_str(&format!(",\"fh\":{fh}"));
    }
    if let Some(offset) = diagnostic.context.offset {
        fields.push_str(&format!(",\"offset\":{offset}"));
    }
    if let Some(size) = diagnostic.context.size {
        fields.push_str(&format!(",\"size\":{size}"));
    }
    if let Some(tag) = diagnostic.context.tag {
        fields.push_str(&format!(",\"tag\":{tag}"));
    }
    fields.push('}');
    fields
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
    use super::{diagnostic_json, Diagnostic, DiagnosticContext, Diagnostics};
    use std::{fs, time::SystemTime};

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
            context: DiagnosticContext {
                path: Some("/runtime/status".to_string()),
                fh: Some(4),
                offset: Some(12),
                size: Some(64),
                tag: Some(2),
            },
        });
        assert!(line.contains("\"event\":\"operation_error\""));
        assert!(line.contains("\"seq\":7"));
        assert!(line.contains("\"path\":\"/runtime/status\""));
        assert!(line.contains("\"fh\":4"));
        assert!(line.contains("\"offset\":12"));
        assert!(line.contains("\"size\":64"));
        assert!(line.contains("\"tag\":2"));
        assert!(line.contains("timed out\\nwhile reading \\\"x\\\""));
    }

    #[test]
    fn diagnostics_file_is_bounded_to_capacity() {
        let path = std::env::temp_dir().join(format!(
            "r9p-fuse-diagnostics-{:?}.jsonl",
            SystemTime::now()
        ));
        let diagnostics = Diagnostics::new(2, Some(path.clone()));

        diagnostics
            .record("one", 1, 1, 1, 0, "first")
            .expect("first diagnostic should record");
        diagnostics
            .record("two", 1, 2, 1, 0, "second")
            .expect("second diagnostic should record");
        diagnostics
            .record("three", 1, 3, 1, 0, "third")
            .expect("third diagnostic should record");

        let content = fs::read_to_string(&path).expect("diagnostics file should exist");
        let _ = fs::remove_file(path);
        assert_eq!(content.lines().count(), 2);
        assert!(!content.contains("\"event\":\"one\""));
        assert!(content.contains("\"event\":\"two\""));
        assert!(content.contains("\"event\":\"three\""));
    }
}
