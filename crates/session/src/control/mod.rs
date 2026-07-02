mod freshness;
mod json;
mod options;
mod query;
mod request;
mod server;
mod snapshot;
mod tree;

use crate::feed::{start_feed_worker, FeedState, FeedWorkerConfig};
use crate::{Client, Error, Result};
pub use request::{parse_request, ControlRequest};
use std::{
    fs,
    path::Path,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::net::UnixListener;

#[derive(Clone, Debug)]
pub struct ControlConfig {
    pub address: String,
    pub uname: String,
    pub aname: String,
    pub msize: u32,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub change_feed_path: Option<String>,
    pub change_feed_stream_path: Option<String>,
    pub change_feed_cursor_template: Option<String>,
    pub change_feed_poll_interval: Duration,
    pub change_feed_backpressure_limit: usize,
}

pub(super) fn status_json(
    client: &Client,
    config: &ControlConfig,
    feed_state: &FeedState,
    session_epoch: &str,
) -> Result<String> {
    let root = client.stat_timeout(client.root_fid(), config.request_timeout)?;
    let feed = feed_state.snapshot();
    let response_freshness = freshness::ResponseFreshness::from_feed(session_epoch, feed_state);
    let mut out = String::from("{\"ok\":true,\"kind\":\"session.status.v1\",\"attached\":true");
    out.push_str(",\"endpoint\":");
    json::push_string(&mut out, &config.address);
    out.push_str(",\"uname\":");
    json::push_string(&mut out, &config.uname);
    out.push_str(",\"aname\":");
    json::push_string(&mut out, &config.aname);
    out.push_str(",\"msize\":");
    out.push_str(&client.msize().to_string());
    out.push_str(",\"root\":{\"qid_path\":");
    out.push_str(&root.qid.path.to_string());
    out.push_str(",\"qid_version\":");
    out.push_str(&root.qid.version.to_string());
    out.push_str(",\"qid_type\":");
    out.push_str(&root.qid.qtype.to_string());
    out.push_str("},\"freshness\":");
    freshness::push_json(&mut out, &response_freshness);
    out.push_str(",\"feed\":{\"state\":");
    json::push_string(&mut out, feed.state);
    out.push_str(",\"source\":");
    match feed.source {
        Some(source) => json::push_string(&mut out, source),
        None => out.push_str("null"),
    }
    out.push_str(",\"last_event_id\":");
    match feed.last_event_id {
        Some(event_id) => json::push_string(&mut out, &event_id),
        None => out.push_str("null"),
    }
    out.push_str(",\"last_generation\":");
    match feed.last_generation {
        Some(generation) => out.push_str(&generation.to_string()),
        None => out.push_str("null"),
    }
    out.push_str(",\"fresh_instance\":");
    out.push_str(if feed.fresh_instance { "true" } else { "false" });
    out.push_str(",\"last_error\":");
    match feed.last_error {
        Some(error) => json::push_string(&mut out, &error),
        None => out.push_str("null"),
    }
    out.push_str("}}");
    Ok(out)
}

#[cfg(unix)]
pub fn serve_control_socket(socket_path: &Path, config: ControlConfig) -> Result<()> {
    if socket_path.exists() {
        fs::remove_file(socket_path)
            .map_err(|error| Error::io(format!("remove {}", socket_path.display()), error))?;
    }
    let listener = UnixListener::bind(socket_path)
        .map_err(|error| Error::io(format!("bind {}", socket_path.display()), error))?;
    let client = Client::connect_with_timeout(
        &config.address,
        &config.uname,
        &config.aname,
        config.msize,
        config.connect_timeout,
    )?;
    let feed_state = FeedState::new();
    let session_epoch = new_session_epoch();
    let _feed_handle = if let Some(path) = config.change_feed_path.clone() {
        Some(start_feed_worker(
            client.clone(),
            FeedWorkerConfig {
                path,
                stream_path: config.change_feed_stream_path.clone(),
                cursor_template: config.change_feed_cursor_template.clone(),
                poll_interval: config.change_feed_poll_interval,
                lookup_timeout: config.request_timeout,
                read_timeout: config.request_timeout,
                control_timeout: config.request_timeout,
                backpressure_limit: config.change_feed_backpressure_limit,
            },
            feed_state.clone(),
        )?)
    } else {
        feed_state.set_disabled();
        None
    };
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let client = client.clone();
                let config = config.clone();
                let feed_state = feed_state.clone();
                let session_epoch = session_epoch.clone();
                thread::spawn(move || {
                    if let Err(error) = server::serve_control_connection(
                        stream,
                        client,
                        config,
                        feed_state,
                        session_epoch,
                    ) {
                        eprintln!("r9p session control connection: {error}");
                    }
                });
            }
            Err(error) => {
                return Err(Error::io(
                    format!("accept {}", socket_path.display()),
                    error,
                ));
            }
        }
    }
    Ok(())
}

fn new_session_epoch() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("session:{}:{nanos}", std::process::id())
}

#[cfg(not(unix))]
pub fn serve_control_socket(_socket_path: &Path, _config: ControlConfig) -> Result<()> {
    Err(Error::new(
        libc::ENOTSUP,
        "session control sockets require Unix sockets",
    ))
}

#[cfg(unix)]
pub fn request_control_socket(
    socket_path: &Path,
    request: &str,
    timeout: Duration,
) -> Result<String> {
    let request = parse_request(request).map_err(|error| Error::new(libc::EINVAL, error))?;
    let address = format!("unix!{}", socket_path.display());
    let client = Client::connect_with_timeout(&address, "session", "", 65_536, timeout)?;
    let path = control_path_for_request(&request);
    let segments = snapshot::parse_namespace_path(&path);
    let fid = if segments.is_empty() {
        client.clone_fid_timeout(client.root_fid(), timeout)?
    } else {
        client.walk_timeout(client.root_fid(), &segments, timeout)?
    };
    if let Err(error) = client.open_timeout(fid, crate::OREAD, timeout) {
        let _ = client.clunk_timeout(fid, timeout);
        return Err(error);
    }
    let read = snapshot::read_all(&client, fid, timeout);
    let clunk = client.clunk_timeout(fid, timeout);
    let bytes = read?;
    clunk?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

#[cfg(not(unix))]
pub fn request_control_socket(
    _socket_path: &Path,
    _request: &str,
    _timeout: Duration,
) -> Result<String> {
    Err(Error::new(
        libc::ENOTSUP,
        "session control sockets require Unix sockets",
    ))
}

fn control_path_for_request(request: &ControlRequest) -> String {
    match request {
        ControlRequest::Status => "/status".to_string(),
        ControlRequest::Snapshot { path, depth } => {
            format!("/snapshot/{depth}{}", control_path_suffix(path))
        }
        ControlRequest::Stat { path } => format!("/stat{}", control_path_suffix(path)),
        ControlRequest::List { path } => format!("/list{}", control_path_suffix(path)),
        ControlRequest::Read { path } => format!("/read{}", control_path_suffix(path)),
    }
}

fn control_path_suffix(path: &str) -> String {
    let segments = snapshot::parse_namespace_path(path);
    if segments.is_empty() {
        "/.".to_string()
    } else {
        format!("/{}", path.trim_matches('/'))
    }
}
