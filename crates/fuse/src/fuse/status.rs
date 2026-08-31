//! Small local status sink for long-running mounts.

use crate::{
    error::{Error, Result},
    fuse::read_cache::CacheSnapshot,
};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

const CACHE_STATUS_INTERVAL: Duration = Duration::from_secs(1);
static STATUS_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub(super) struct MountStatus {
    state: Arc<Mutex<State>>,
}

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
    generation: &'static str,
    publish_enabled: bool,
}

impl MountStatus {
    pub(super) fn new(
        path: Option<PathBuf>,
        namespace_source: String,
        active_endpoint: String,
        endpoint_candidates: Vec<String>,
    ) -> Self {
        Self {
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
                generation: "preparing",
                publish_enabled: false,
            })),
        }
    }

    pub(super) fn activate(&self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.generation = "active";
        state.publish_enabled = true;
        let _ = write_status(&state);
    }

    pub(super) fn set_transport(&self, active_endpoint: String) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.active_endpoint = active_endpoint;
        publish_status(&state);
    }

    pub(super) fn set_change_feed(
        &self,
        change_feed: &'static str,
        source: Option<&'static str>,
        last_event_id: Option<String>,
        last_error: Option<String>,
    ) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.change_feed = change_feed;
        state.source = source;
        if last_event_id.is_some() {
            state.last_event_id = last_event_id;
        }
        state.last_error = last_error;
        publish_status(&state);
    }

    pub(super) fn set_read_cache(&self, read_cache: CacheSnapshot) {
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
        publish_status(&state);
    }

    pub(super) fn retire(&self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.generation = "retired";
        let _ = write_status(&state);
        state.publish_enabled = false;
    }
}

fn write_status(state: &State) -> Result<()> {
    let Some(path) = state.path.as_ref() else {
        return Ok(());
    };
    let parent = path
        .parent()
        .ok_or_else(|| Error::new(libc::EINVAL, "status path parent missing"))?;
    let sequence = STATUS_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".r9p-mount-status-{}-{sequence}.tmp",
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o666)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)
        .map_err(|error| Error::io(format!("create status {}", temporary.display()), error))?;
    if let Err(error) = writeln!(file, "{}", status_json(state)) {
        let _ = fs::remove_file(&temporary);
        return Err(Error::io(
            format!("write status {}", temporary.display()),
            error,
        ));
    }
    drop(file);
    fs::rename(&temporary, path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        Error::io(format!("publish status {}", path.display()), error)
    })
}

fn publish_status(state: &State) {
    if state.publish_enabled {
        let _ = write_status(state);
    }
}

fn status_json(state: &State) -> String {
    format!(
        "{{\"namespace_source\":\"{}\",\"process_id\":{},\"mount_generation\":\"{}\",\"active_endpoint\":\"{}\",\"endpoint_candidates\":{},\"change_feed\":\"{}\",\"source\":{},\"last_event_id\":{},\"last_error\":{},\"persistent_read_cache\":{}}}",
        escape_json(&state.namespace_source),
        std::process::id(),
        state.generation,
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
    use super::{status_json, write_status, CacheSnapshot, State};
    use std::{
        fs::{self, File},
        io::Read,
        sync::atomic::{AtomicU64, Ordering},
    };

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

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
            generation: "active",
            publish_enabled: true,
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

    #[test]
    fn status_replacement_keeps_open_readers_on_one_complete_snapshot() {
        let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "r9p-mount-status-{}-{sequence}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir(&directory).expect("create status directory");
        let path = directory.join("status");
        let mut state = State {
            path: Some(path.clone()),
            namespace_source: "/sources/newsgroups/downloads/files".to_string(),
            active_endpoint: "nucbox.mesh:9564".to_string(),
            endpoint_candidates: vec!["nucbox.mesh:9564".to_string()],
            change_feed: "connected",
            source: Some("session"),
            last_event_id: Some("event-1".to_string()),
            last_error: None,
            read_cache: None,
            last_cache_publish: None,
            generation: "active",
            publish_enabled: true,
        };
        write_status(&state).expect("publish first status");
        let first_json = status_json(&state) + "\n";
        let mut first_reader = File::open(&path).expect("open first status");

        state.active_endpoint = "m7.mesh:9564".to_string();
        state.last_event_id = Some("event-2".to_string());
        write_status(&state).expect("publish replacement status");
        let second_json = status_json(&state) + "\n";
        let mut retained = String::new();
        first_reader
            .read_to_string(&mut retained)
            .expect("read retained status");

        assert_eq!(retained, first_json);
        assert_eq!(
            fs::read_to_string(&path).expect("read current status"),
            second_json
        );
        fs::remove_dir_all(directory).expect("remove status directory");
    }

    #[test]
    fn retired_generation_cannot_overwrite_successor_status() {
        let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "r9p-retired-mount-status-{}-{sequence}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir(&directory).expect("create status directory");
        let path = directory.join("status");
        let status = super::MountStatus::new(
            Some(path.clone()),
            "/sources/newsgroups/downloads/files".to_string(),
            "nucbox.mesh:9564".to_string(),
            vec!["nucbox.mesh:9564".to_string()],
        );
        status.activate();
        status.retire();
        let retired = fs::read_to_string(&path).expect("read retired status");
        assert!(retired.contains("\"mount_generation\":\"retired\""));

        fs::write(&path, "successor\n").expect("publish successor status");
        status.set_transport("m7.mesh:9564".to_string());
        assert_eq!(
            fs::read_to_string(&path).expect("read successor status"),
            "successor\n"
        );
        fs::remove_dir_all(directory).expect("remove status directory");
    }
}
