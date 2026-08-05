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
    connect_stream_expecting(config, connect_timeout, None)
}

/// `expected_responder` is the service name a referral said would answer. It is
/// a private seam on purpose: out-of-tree consumers construct `ConnectionConfig`
/// directly, so a new required field there breaks every one of them. Root
/// connects pass `None` and fall back to the name in the auth config.
pub(crate) fn connect_stream_expecting(
    config: &ConnectionConfig,
    connect_timeout: Duration,
    expected_responder: Option<&str>,
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
                    // The domain comes from the client config. A certified
                    // config that names no service is dialled through
                    // authenticate_client_to by the caller that knows which
                    // service it wants; it is deliberately not a field on the
                    // public ConnectionConfig, because out-of-tree consumers
                    // construct that struct and a new required field breaks
                    // every one of them.
                    match (expected_responder, auth.domain()) {
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
                            "this auth config names no service and no referral supplied one",
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

#[cfg(test)]
mod referral_binding_tests {
    //! Proves the name derived from a referral's `authority_boundary` actually
    //! reaches the handshake. The auth crate already proves
    //! `authenticate_client_to` checks it; what needs covering here is the
    //! private plumbing in between, which no other test touches.
    use super::*;
    use r9p::export_descriptor::AuthBoundary;
    use r9p_auth::{
        authenticate_server, generate_key_pair, generate_root_key_pair, Certificate,
        CertificateBody, RootKeyPair, ServerConfig,
    };
    use std::{
        net::{TcpListener, TcpStream},
        sync::atomic::{AtomicU64, Ordering},
        thread,
    };

    static SERIAL: AtomicU64 = AtomicU64::new(0);
    const FROM: u64 = 1_000;
    const UNTIL: u64 = 4_000_000_000;

    fn cert(root: &RootKeyPair, key: r9p_auth::PublicKey, name: &str) -> Certificate {
        let body = CertificateBody::new(name, key, Vec::<String>::new(), FROM, UNTIL, root.public)
            .expect("certificate body");
        Certificate::sign(&root.private, body).expect("sign")
    }

    /// Spawns an XX responder certified as `served_name` and returns a client
    /// config that pins nothing, plus the address.
    fn responder(served_name: &str) -> (String, PathBuf, RootKeyPair) {
        let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
        let dir = env::temp_dir().join(format!("r9p-referral-{}-{serial}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let root = generate_root_key_pair().expect("root");
        let server_key = generate_key_pair().expect("server key");
        let client_key = generate_key_pair().expect("client key");

        let server = ServerConfig::new_with_roots(
            served_name,
            server_key.private,
            Vec::new(),
            [root.public],
        )
        .expect("server config")
        .with_certificate(cert(&root, server_key.public, served_name))
        .expect("server certificate");

        let listener = TcpListener::bind("127.0.0.1:0").expect("listen");
        let address = listener.local_addr().expect("addr").to_string();
        thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let _ = authenticate_server(stream, &server, Duration::from_secs(2));
            }
        });

        let key_path = dir.join("client.key");
        r9p_auth::write_key_pair(&key_path, &dir.join("client.pub"), &client_key).expect("keys");
        let cert_path = dir.join("client.crt");
        cert(&root, client_key.public, "op")
            .write(&cert_path)
            .expect("write cert");
        let conf = dir.join("client.conf");
        std::fs::write(
            &conf,
            format!(
                "format r9p-session-auth.v1\nrole client\nprivate-key {}\ncertificate {}\nroot {}\n",
                key_path.display(),
                cert_path.display(),
                root.public
            ),
        )
        .expect("write conf");
        (address, conf, root)
    }

    fn dial(address: &str, conf: &PathBuf, expected: Option<&str>) -> Result<ClientStream> {
        let config = ConnectionConfig {
            address: address.to_string(),
            uname: "op".to_string(),
            aname: "/".to_string(),
            msize: 65_536,
            auth_config: Some(conf.clone()),
            authorities: crate::AuthorityBindings::new(),
        };
        connect_stream_expecting(&config, Duration::from_secs(2), expected)
    }

    #[test]
    fn a_referral_derived_name_reaches_the_handshake_and_is_accepted() {
        let (address, conf, _root) = responder("terminal-m7");
        // Exactly what the referral path derives.
        let boundary = AuthBoundary::parse("p9any:noise-xx@terminal-m7").expect("boundary");
        let expected = boundary.p9any_domain().expect("domain");
        assert_eq!(expected, "terminal-m7");
        assert!(dial(&address, &conf, Some(expected)).is_ok());
    }

    #[test]
    fn a_validly_certified_responder_under_another_name_is_rejected() {
        // The responder holds a genuine certificate from the same root; only the
        // name differs from what the referral promised.
        let (address, conf, _root) = responder("terminal-nucbox");
        let boundary = AuthBoundary::parse("p9any:noise-xx@terminal-m7").expect("boundary");
        let expected = boundary.p9any_domain().expect("domain");
        assert!(dial(&address, &conf, Some(expected)).is_err());
    }
}
