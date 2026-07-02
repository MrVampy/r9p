mod json;
mod request;
mod server;
mod snapshot;
mod tree;

use crate::{Client, Error, Result};
pub use request::{parse_request, ControlRequest};
use std::{fs, path::Path, thread, time::Duration};

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
}

pub(super) fn status_json(client: &Client, config: &ControlConfig) -> Result<String> {
    let root = client.stat_timeout(client.root_fid(), config.request_timeout)?;
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
    out.push_str("},\"freshness\":{\"state\":\"fresh\"}}");
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
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let client = client.clone();
                let config = config.clone();
                thread::spawn(move || {
                    if let Err(error) = server::serve_control_connection(stream, client, config) {
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
