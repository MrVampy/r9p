use crate::{error::client_error, ConnectionConfig, Error, Result};
use r9p::{blocking, multiplex::MultiplexTransport};
use r9p_auth::{
    authenticate_client, authenticate_client_to, ClientConfig as AuthConfig, SecureStream,
};
use std::{
    env,
    io::{self, Read, Write},
    net::{Shutdown, TcpStream},
    path::{Path, PathBuf},
    time::Duration,
};

const TCP_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_AUTH_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

#[cfg(unix)]
use std::os::unix::net::UnixStream;

pub(crate) enum ClientStream {
    Tcp(TcpStream),
    Secure(SecureStream),
    #[cfg(unix)]
    Unix(UnixStream),
}

impl Read for ClientStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.read(buffer),
            Self::Secure(stream) => stream.read(buffer),
            #[cfg(unix)]
            Self::Unix(stream) => stream.read(buffer),
        }
    }
}

impl Write for ClientStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.write(buffer),
            Self::Secure(stream) => stream.write(buffer),
            #[cfg(unix)]
            Self::Unix(stream) => stream.write(buffer),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.flush(),
            Self::Secure(stream) => stream.flush(),
            #[cfg(unix)]
            Self::Unix(stream) => stream.flush(),
        }
    }
}

impl MultiplexTransport for ClientStream {
    fn try_clone_transport(&self) -> io::Result<Self> {
        match self {
            Self::Tcp(stream) => stream.try_clone().map(Self::Tcp),
            Self::Secure(stream) => stream.try_clone().map(Self::Secure),
            #[cfg(unix)]
            Self::Unix(stream) => stream.try_clone().map(Self::Unix),
        }
    }

    fn shutdown_transport(&self) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.shutdown(Shutdown::Both),
            Self::Secure(stream) => stream.shutdown(),
            #[cfg(unix)]
            Self::Unix(stream) => stream.shutdown(Shutdown::Both),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConnectTarget {
    Tcp(String),
    Unix(PathBuf),
}

pub(crate) fn connect_stream(
    config: &ConnectionConfig,
    connect_timeout: Duration,
) -> Result<ClientStream> {
    match parse_connection_target(&config.address)? {
        ConnectTarget::Tcp(socket) => {
            let stream = blocking::connect_tcp_stream(&socket).map_err(client_error)?;
            stream
                .set_write_timeout(Some(TCP_WRITE_TIMEOUT))
                .map_err(|error| Error::io("set TCP write timeout", error))?;
            match &config.auth_config {
                Some(path) => {
                    let auth = AuthConfig::read(path).map_err(client_error)?;
                    let handshake_timeout = if connect_timeout.is_zero() {
                        DEFAULT_AUTH_HANDSHAKE_TIMEOUT
                    } else {
                        connect_timeout
                    };
                    match (&config.auth_domain, auth.domain()) {
                        (Some(domain), _) => authenticate_client_to(
                            stream,
                            &auth,
                            domain,
                            &config.uname,
                            handshake_timeout,
                        ),
                        (None, Some(_)) => {
                            authenticate_client(stream, &auth, &config.uname, handshake_timeout)
                        }
                        (None, None) => Err(r9p::error::Error::from(
                            "this auth config names no service; supply an auth domain",
                        )),
                    }
                    .map(ClientStream::Secure)
                    .map_err(client_error)
                }
                None => Ok(ClientStream::Tcp(stream)),
            }
        }
        ConnectTarget::Unix(path) if config.auth_config.is_none() => connect_unix_stream(&path),
        ConnectTarget::Unix(_) => Err(Error::new(
            libc::EINVAL,
            "session auth config is only valid for TCP endpoints",
        )),
    }
}

pub(crate) fn parse_connection_target(address: &str) -> Result<ConnectTarget> {
    if let Some(path) = address.strip_prefix("unix!") {
        return parse_unix_target(path);
    }
    if let Some(service) = address.strip_prefix("namespace!") {
        let namespace = env::var("NAMESPACE")
            .map_err(|_| Error::new(libc::EINVAL, "NAMESPACE is required for namespace!"))?;
        return namespace_service_path(Path::new(&namespace), service).map(ConnectTarget::Unix);
    }
    blocking::parse_tcp_address(address)
        .map(ConnectTarget::Tcp)
        .map_err(|error| Error::new(libc::EINVAL, error.display_lossy().to_string()))
}

fn parse_unix_target(path: &str) -> Result<ConnectTarget> {
    if path.is_empty() {
        return Err(Error::new(libc::EINVAL, "unix! address requires a path"));
    }
    Ok(ConnectTarget::Unix(PathBuf::from(path)))
}

pub(crate) fn namespace_service_path(namespace: &Path, service: &str) -> Result<PathBuf> {
    if service.is_empty() {
        return Err(Error::new(
            libc::EINVAL,
            "namespace! address requires a service",
        ));
    }
    if service.contains('/') {
        return Err(Error::new(
            libc::EINVAL,
            "namespace! service must be a single path element",
        ));
    }
    if namespace.as_os_str().is_empty() {
        return Err(Error::new(libc::EINVAL, "NAMESPACE must not be empty"));
    }
    Ok(namespace.join(service))
}

#[cfg(unix)]
fn connect_unix_stream(path: &Path) -> Result<ClientStream> {
    UnixStream::connect(path)
        .map(ClientStream::Unix)
        .map_err(|error| Error::io(format!("connect {}", path.display()), error))
}

#[cfg(not(unix))]
fn connect_unix_stream(path: &Path) -> Result<ClientStream> {
    Err(Error::new(
        libc::ENOSYS,
        format!(
            "unix sockets are not supported on this platform: {}",
            path.display()
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::{namespace_service_path, parse_connection_target, ConnectTarget};
    use std::{path::Path, path::PathBuf};

    #[test]
    fn parses_unix_address() {
        let parsed = parse_connection_target("unix!/tmp/r9p.sock").expect("address should parse");
        assert_eq!(parsed, ConnectTarget::Unix("/tmp/r9p.sock".into()));
    }

    #[test]
    fn resolves_namespace_service_under_namespace_dir() {
        let path = namespace_service_path(Path::new("/tmp/namespace"), "example-service")
            .expect("service should resolve");
        assert_eq!(path, PathBuf::from("/tmp/namespace/example-service"));
    }

    #[test]
    fn rejects_namespace_service_paths() {
        let error = namespace_service_path(Path::new("/tmp/namespace"), "example/service")
            .expect_err("service path should be rejected");
        assert_eq!(error.errno, libc::EINVAL);
        assert!(error.message().contains("single path element"));
    }
}
