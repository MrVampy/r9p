mod freshness;
mod json;
mod options;
mod query;
mod request;
mod server;
mod snapshot;
mod snapshot_path;
mod snapshot_report;
mod tree;

use crate::feed::{start_feed_worker, FeedEventBus, FeedState, FeedWorkerConfig, FeedWorkerHandle};
use crate::{Client, ClientSlot, ConnectionConfig, Error, NamespaceCache, Result, SessionEpoch};
pub use request::{parse_request, ControlRequest};
use std::{
    fs,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::net::UnixListener;

#[derive(Clone, Debug)]
pub struct ControlConfig {
    pub address: String,
    pub uname: String,
    pub aname: String,
    pub msize: u32,
    pub auth_config: Option<PathBuf>,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub change_feed_path: Option<String>,
    pub change_feed_stream_path: Option<String>,
    pub change_feed_cursor_template: Option<String>,
    pub change_feed_poll_interval: Duration,
    pub change_feed_backpressure_limit: usize,
}

pub struct ControlRuntime {
    client: ClientSlot,
    feed_state: FeedState,
    cache: NamespaceCache,
    session_epoch: SessionEpoch,
    feed_events: FeedEventBus,
    _feed_handle: Option<FeedWorkerHandle>,
}

impl ControlRuntime {
    pub fn start(config: &ControlConfig) -> Result<Self> {
        let client = Client::connect_with_timeout(
            &ConnectionConfig {
                address: config.address.clone(),
                uname: config.uname.clone(),
                aname: config.aname.clone(),
                msize: config.msize,
                auth_config: config.auth_config.clone(),
            },
            config.connect_timeout,
        )?;
        let session_epoch = SessionEpoch::new();
        let client = ClientSlot::new_with_epoch(client, session_epoch.clone());
        let feed_state = FeedState::new();
        let cache = NamespaceCache::new();
        let feed_events = FeedEventBus::new(config.change_feed_backpressure_limit);
        let feed_handle = if let Some(path) = config.change_feed_path.clone() {
            Some(start_feed_worker(
                client.clone(),
                FeedWorkerConfig {
                    path,
                    stream_path: config.change_feed_stream_path.clone(),
                    cursor_template: config.change_feed_cursor_template.clone(),
                    cache: Some(cache.clone()),
                    event_bus: Some(feed_events.clone()),
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
        Ok(Self {
            client,
            feed_state,
            cache,
            session_epoch,
            feed_events,
            _feed_handle: feed_handle,
        })
    }

    pub fn client_slot(&self) -> ClientSlot {
        self.client.clone()
    }

    pub fn feed_events(&self) -> FeedEventBus {
        self.feed_events.clone()
    }
}

pub(super) fn status_json(
    client: &Client,
    config: &ControlConfig,
    feed_state: &FeedState,
    cache: &NamespaceCache,
    session_epoch: &SessionEpoch,
) -> Result<String> {
    let root = client.stat_timeout(client.root_fid(), config.request_timeout)?;
    let feed = feed_state.snapshot();
    let cache_stats = cache.stats();
    let response_freshness = freshness::ResponseFreshness::from_feed(session_epoch, feed_state)?;
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
    out.push_str("},\"cache\":{\"entries\":");
    out.push_str(&cache_stats.entries.to_string());
    out.push_str(",\"directories\":");
    out.push_str(&cache_stats.directories.to_string());
    out.push_str(",\"stale_entries\":");
    out.push_str(&cache_stats.stale_entries.to_string());
    out.push_str("}}");
    Ok(out)
}

#[cfg(unix)]
pub fn serve_control_socket(socket_path: &Path, config: ControlConfig) -> Result<()> {
    let runtime = ControlRuntime::start(&config)?;
    serve_control_socket_with_runtime(socket_path, config, runtime)
}

#[cfg(unix)]
pub fn serve_control_socket_with_runtime(
    socket_path: &Path,
    config: ControlConfig,
    runtime: ControlRuntime,
) -> Result<()> {
    if socket_path.exists() {
        fs::remove_file(socket_path)
            .map_err(|error| Error::io(format!("remove {}", socket_path.display()), error))?;
    }
    let listener = UnixListener::bind(socket_path)
        .map_err(|error| Error::io(format!("bind {}", socket_path.display()), error))?;
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let client = runtime.client.clone();
                let config = config.clone();
                let feed_state = runtime.feed_state.clone();
                let cache = runtime.cache.clone();
                let session_epoch = runtime.session_epoch.clone();
                thread::spawn(move || {
                    if let Err(error) = server::serve_control_connection(
                        stream,
                        client,
                        config,
                        feed_state,
                        cache,
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
    let client = Client::connect_with_timeout(
        &ConnectionConfig {
            address,
            uname: "session".to_string(),
            aname: String::new(),
            msize: 65_536,
            auth_config: None,
        },
        timeout,
    )?;
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
