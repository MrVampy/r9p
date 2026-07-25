use std::{
    path::Path,
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::net::UnixStream;

use r9p::{
    blocking::{self, ConnectionTimeouts, ReadWrite},
    codec,
    message::RMessage,
};
use r9p_auth::{authenticate_client, ClientConfig as AuthConfig};

use crate::errors::{cli_error, CliResult};
use crate::target::{namespace_socket, split_namespace_path, Target};

pub(crate) fn dial_target(target: &Target) -> CliResult<Box<dyn ReadWrite>> {
    match &target.config.address {
        Some(address) => dial_address(address, &target.config),
        None => {
            if target.config.auth_config.is_some() {
                return Err(cli_error(
                    "--auth-config requires a TCP endpoint supplied with -a or --bind",
                ));
            }
            let (service, _) = split_namespace_path(&target.path)?;
            let socket = namespace_socket(&service)?;
            dial_unix_socket(&socket, target.config.request_timeout)
        }
    }
}

pub(crate) fn dial_address(
    address: &str,
    config: &crate::target::Config,
) -> CliResult<Box<dyn ReadWrite>> {
    if let Some(path) = unix_address_path(address) {
        if config.auth_config.is_some() {
            return Err(cli_error("--auth-config is not valid for a Unix endpoint"));
        }
        return dial_unix_socket(Path::new(path), config.request_timeout);
    }
    let stream = match config.request_timeout {
        Some(timeout) => blocking::connect_tcp_stream_with_timeouts(
            address,
            ConnectionTimeouts::new(timeout, timeout, timeout),
        )?,
        None => blocking::connect_tcp_stream(address)?,
    };
    match &config.auth_config {
        Some(path) => {
            let auth = AuthConfig::read(path)?;
            let timeout = config
                .request_timeout
                .unwrap_or_else(|| Duration::from_secs(30));
            let stream = authenticate_client(stream, &auth, &config.uname, timeout)?;
            Ok(Box::new(stream))
        }
        None => Ok(Box::new(stream)),
    }
}

fn unix_address_path(address: &str) -> Option<&str> {
    address
        .strip_prefix("unix!")
        .or_else(|| address.strip_prefix("unix:"))
}

#[cfg(unix)]
pub(crate) fn dial_unix_socket(
    path: &Path,
    request_timeout: Option<Duration>,
) -> CliResult<Box<dyn ReadWrite>> {
    let stream = UnixStream::connect(path)
        .map_err(|error| cli_error(format!("connect {}: {error}", path.display())))?;
    apply_unix_timeout(&stream, request_timeout)?;
    Ok(Box::new(stream))
}

#[cfg(unix)]
fn apply_unix_timeout(stream: &UnixStream, request_timeout: Option<Duration>) -> CliResult<()> {
    stream
        .set_read_timeout(request_timeout)
        .map_err(|error| cli_error(format!("set read timeout: {error}")))?;
    stream
        .set_write_timeout(request_timeout)
        .map_err(|error| cli_error(format!("set write timeout: {error}")))
}

#[cfg(not(unix))]
pub(crate) fn dial_unix_socket(
    path: &Path,
    _request_timeout: Option<Duration>,
) -> CliResult<Box<dyn ReadWrite>> {
    Err(cli_error(format!(
        "unix sockets are not supported on this platform: {}",
        path.display()
    )))
}

pub(crate) fn read_response(
    stream: &mut Box<dyn ReadWrite>,
    max_frame_size: u32,
) -> CliResult<RMessage> {
    codec::read_rmessage_checked(stream, max_frame_size)?
        .ok_or_else(|| cli_error("9P transport closed before response"))
}

#[cfg(test)]
mod tests {
    use super::unix_address_path;

    #[test]
    fn accepts_plan9_and_descriptor_unix_address_forms() {
        assert_eq!(
            unix_address_path("unix!/tmp/r9p.sock"),
            Some("/tmp/r9p.sock")
        );
        assert_eq!(
            unix_address_path("unix:/tmp/r9p.sock"),
            Some("/tmp/r9p.sock")
        );
        assert_eq!(unix_address_path("127.0.0.1:564"), None);
    }
}
