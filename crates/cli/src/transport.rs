use std::{net::TcpStream, path::Path, time::Duration};

#[cfg(unix)]
use std::os::unix::net::UnixStream;

use r9p::{
    blocking::{self, ReadWrite},
    codec,
    message::RMessage,
};

use crate::errors::{cli_error, CliResult};
use crate::target::{namespace_socket, split_namespace_path, Target};

pub(crate) fn dial_target(target: &Target) -> CliResult<Box<dyn ReadWrite>> {
    match &target.config.address {
        Some(address) => dial_address(address, target.config.request_timeout),
        None => {
            let (service, _) = split_namespace_path(&target.path)?;
            let socket = namespace_socket(&service)?;
            dial_unix_socket(&socket, target.config.request_timeout)
        }
    }
}

pub(crate) fn dial_address(
    address: &str,
    request_timeout: Option<Duration>,
) -> CliResult<Box<dyn ReadWrite>> {
    if let Some(path) = unix_address_path(address) {
        return dial_unix_socket(Path::new(path), request_timeout);
    }
    let socket = blocking::parse_tcp_address(address)?;
    let stream = TcpStream::connect(&socket)
        .map_err(|error| cli_error(format!("connect {socket}: {error}")))?;
    stream
        .set_nodelay(true)
        .map_err(|error| cli_error(format!("set TCP_NODELAY: {error}")))?;
    apply_tcp_timeout(&stream, request_timeout)?;
    Ok(Box::new(stream))
}

fn apply_tcp_timeout(stream: &TcpStream, request_timeout: Option<Duration>) -> CliResult<()> {
    stream
        .set_read_timeout(request_timeout)
        .map_err(|error| cli_error(format!("set read timeout: {error}")))?;
    stream
        .set_write_timeout(request_timeout)
        .map_err(|error| cli_error(format!("set write timeout: {error}")))
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

pub(crate) fn read_response(stream: &mut Box<dyn ReadWrite>) -> CliResult<RMessage> {
    let mut prefix = [0_u8; 4];
    stream.read_exact(&mut prefix)?;
    let size = u32::from_le_bytes(prefix);
    if size < codec::FRAME_HEADER_SIZE {
        return Err(cli_error(format!("short 9P frame {size}")));
    }
    let rest_len = usize::try_from(size - 4)?;
    let mut frame = Vec::with_capacity(rest_len + 4);
    frame.extend(prefix);
    frame.resize(rest_len + 4, 0);
    stream.read_exact(&mut frame[4..])?;
    Ok(codec::decode_rmessage(&frame)?)
}

#[cfg(test)]
mod tests {
    use super::unix_address_path;

    #[test]
    fn accepts_legacy_and_descriptor_unix_address_forms() {
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
