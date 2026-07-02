mod json;
mod snapshot;

use crate::{Client, Error, Result};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};

#[derive(Clone, Debug)]
pub struct ControlConfig {
    pub address: String,
    pub uname: String,
    pub aname: String,
    pub msize: u32,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlRequest {
    Status,
    Snapshot { path: String, depth: usize },
}

pub fn parse_request(line: &str) -> std::result::Result<ControlRequest, String> {
    let fields = line
        .trim_end_matches(['\r', '\n'])
        .split('\t')
        .collect::<Vec<_>>();
    match fields.as_slice() {
        ["status"] => Ok(ControlRequest::Status),
        ["snapshot", path, depth] => {
            let depth = depth
                .parse::<usize>()
                .map_err(|_| format!("invalid snapshot depth {depth}"))?;
            Ok(ControlRequest::Snapshot {
                path: (*path).to_string(),
                depth,
            })
        }
        ["snapshot", path] => Ok(ControlRequest::Snapshot {
            path: (*path).to_string(),
            depth: 1,
        }),
        [command, ..] => Err(format!("unknown session control request {command}")),
        [] => Err("empty session control request".to_string()),
    }
}

pub fn response_json(client: &Client, config: &ControlConfig, request: ControlRequest) -> String {
    match response_json_result(client, config, request) {
        Ok(response) => response,
        Err(error) => json::error_response("session_error", error.message()),
    }
}

fn response_json_result(
    client: &Client,
    config: &ControlConfig,
    request: ControlRequest,
) -> Result<String> {
    match request {
        ControlRequest::Status => status_json(client, config),
        ControlRequest::Snapshot { path, depth } => {
            snapshot::snapshot_json(client, &path, depth, config.request_timeout)
        }
    }
}

fn status_json(client: &Client, config: &ControlConfig) -> Result<String> {
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
            Ok(mut stream) => {
                let response = handle_control_stream(&client, &config, &mut stream);
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.write_all(b"\n");
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
fn handle_control_stream(
    client: &Client,
    config: &ControlConfig,
    stream: &mut UnixStream,
) -> String {
    let mut request = String::new();
    if let Err(error) = stream.read_to_string(&mut request) {
        return json::error_response("read_request", &error.to_string());
    }
    match parse_request(&request) {
        Ok(request) => response_json(client, config, request),
        Err(error) => json::error_response("bad_request", &error),
    }
}

#[cfg(unix)]
pub fn request_control_socket(
    socket_path: &Path,
    request: &str,
    timeout: Duration,
) -> Result<String> {
    let mut stream = UnixStream::connect(socket_path)
        .map_err(|error| Error::io(format!("connect {}", socket_path.display()), error))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|error| Error::io("set control read timeout", error))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|error| Error::io("set control write timeout", error))?;
    stream
        .write_all(request.as_bytes())
        .map_err(|error| Error::io("write control request", error))?;
    stream
        .write_all(b"\n")
        .map_err(|error| Error::io("write control request newline", error))?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|error| Error::io("finish control request", error))?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|error| Error::io("read control response", error))?;
    Ok(response)
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

#[cfg(test)]
mod tests {
    use super::{parse_request, ControlRequest};

    #[test]
    fn parses_status_request() {
        assert_eq!(parse_request("status\n"), Ok(ControlRequest::Status));
    }

    #[test]
    fn parses_snapshot_request() {
        assert_eq!(
            parse_request("snapshot\t/srv\t2\n"),
            Ok(ControlRequest::Snapshot {
                path: "/srv".to_string(),
                depth: 2
            })
        );
    }
}
