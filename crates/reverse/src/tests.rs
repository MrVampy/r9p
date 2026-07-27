use std::{
    fs,
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::atomic::AtomicBool,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use r9p::{
    blocking::Client,
    codec::Variant,
    error::EPERM,
    qid::Qid,
    server::{
        ConnectionHandler, OpenFile, ReadData, ServerCompletion, ServerConfig as R9pServerConfig,
        ServerRequest, ServerRequestKind,
    },
    stat::Stat,
};
use r9p_auth::{
    authenticate_client, authenticate_server, generate_key_pair, ClientConfig, ServerConfig,
};

use super::{
    BrokerConfig, FilesystemExport, FilesystemExportConfig, ProxyEndpoint, ReverseBroker,
    ReverseExport, ReverseExportConfig, SessionProxy, SessionProxyConfig,
};

#[test]
fn reverse_transport_socket_disables_nagle() -> Result<(), Box<dyn std::error::Error>> {
    let listener = std::net::TcpListener::bind(address(Ipv4Addr::LOCALHOST, 0))?;
    let client = std::net::TcpStream::connect(listener.local_addr()?)?;
    let (server, _) = listener.accept()?;

    super::configure_transport_socket(&client)?;
    super::configure_transport_socket(&server)?;

    assert!(client.nodelay()?);
    assert!(server.nodelay()?);
    let client_socket = socket2::SockRef::from(&client);
    let server_socket = socket2::SockRef::from(&server);
    assert!(client_socket.keepalive()?);
    assert!(server_socket.keepalive()?);
    assert_eq!(client_socket.tcp_keepalive_time()?, super::KEEPALIVE_IDLE);
    assert_eq!(server_socket.tcp_keepalive_time()?, super::KEEPALIVE_IDLE);
    #[cfg(target_os = "linux")]
    {
        assert_eq!(
            client_socket.tcp_keepalive_interval()?,
            super::KEEPALIVE_INTERVAL
        );
        assert_eq!(
            server_socket.tcp_keepalive_interval()?,
            super::KEEPALIVE_INTERVAL
        );
        assert_eq!(
            client_socket.tcp_keepalive_retries()?,
            super::KEEPALIVE_RETRIES
        );
        assert_eq!(
            server_socket.tcp_keepalive_retries()?,
            super::KEEPALIVE_RETRIES
        );
    }
    Ok(())
}

#[test]
fn session_proxy_terminates_client_auth_behind_a_local_endpoint(
) -> Result<(), Box<dyn std::error::Error>> {
    let service_key = generate_key_pair()?;
    let compute_key = generate_key_pair()?;
    let upstream = std::net::TcpListener::bind(address(Ipv4Addr::LOCALHOST, 0))?;
    let upstream_endpoint = upstream.local_addr()?;
    let service_auth = ServerConfig::new(
        "r9p-session-proxy-test",
        service_key.private,
        [(
            compute_key.public,
            "/srv/agents/compute/m7".to_string(),
        )],
    )?;
    let server = thread::spawn(move || -> Result<(), r9p::Error> {
        let (stream, _) = upstream.accept()?;
        let session = authenticate_server(stream, &service_auth, Duration::from_secs(2))?;
        assert_eq!(session.peer.principal(), "/srv/agents/compute/m7");
        let mut stream = session.stream;
        let mut request = [0_u8; 4];
        stream.read_exact(&mut request)?;
        assert_eq!(&request, b"ping");
        stream.write_all(b"pong")?;
        stream.shutdown()?;
        Ok(())
    });
    let proxy = SessionProxy::start(SessionProxyConfig {
        bind: ProxyEndpoint::tcp(address(Ipv4Addr::LOCALHOST, 0)),
        upstream: upstream_endpoint,
        auth: ClientConfig::new(
            "r9p-session-proxy-test",
            compute_key.private,
            service_key.public,
        )?,
        principal: "/srv/agents/compute/m7".to_string(),
        max_sessions: 2,
        connect_timeout: Duration::from_secs(2),
        authentication_timeout: Duration::from_secs(2),
    })?;
    let endpoint = proxy
        .endpoint()
        .as_tcp()
        .ok_or("session proxy did not expose TCP")?;
    let mut local = std::net::TcpStream::connect(endpoint)?;
    local.write_all(b"ping")?;
    let mut response = [0_u8; 4];
    local.read_exact(&mut response)?;
    assert_eq!(&response, b"pong");
    server.join().map_err(|_| "session proxy server panicked")??;
    Ok(())
}

#[test]
fn reverse_export_serves_a_writable_file_tree() -> Result<(), Box<dyn std::error::Error>> {
    let server = generate_key_pair()?;
    let client = generate_key_pair()?;
    let broker = ReverseBroker::start(BrokerConfig {
        reverse_bind: address(Ipv4Addr::LOCALHOST, 0),
        proxy_bind: ProxyEndpoint::tcp(address(Ipv4Addr::LOCALHOST, 0)),
        auth: ServerConfig::new(
            "r9p-reverse-test",
            server.private,
            [(client.public, "laptop".to_string())],
        )?,
        peer_principal: "laptop".to_string(),
        max_waiting_streams: 4,
        authentication_timeout: Duration::from_secs(2),
        proxy_wait_timeout: Duration::from_secs(2),
    })?;
    let root = TestRoot::new()?;
    fs::write(root.path.join("specimen.txt"), b"from laptop\n")?;
    let export = FilesystemExport::start(FilesystemExportConfig {
        broker_endpoint: broker.reverse_endpoint(),
        auth: ClientConfig::new("r9p-reverse-test", client.private, server.public)?,
        principal: "laptop".to_string(),
        root: root.path.clone(),
        writable: true,
        connection_pool: 4,
        connect_timeout: Duration::from_secs(2),
        authentication_timeout: Duration::from_secs(2),
        reconnect_min_delay: Duration::from_millis(25),
        reconnect_max_delay: Duration::from_millis(200),
        msize: 65_536,
        max_fids: 256,
    })?;
    wait_ready(&broker, &export)?;

    let mut reader = Client::connect_with_variant(
        std::net::TcpStream::connect(tcp_proxy_endpoint(&broker)?)?,
        "codex",
        "/",
        65_536,
        Variant::R,
    )
    .map_err(|error| stage_error("connect reader", error))?;
    assert_eq!(
        reader
            .read_path("/specimen.txt")
            .map_err(|error| stage_error("read specimen", error))?,
        b"from laptop\n"
    );
    drop(reader);

    wait_for_waiting_stream(&broker)?;
    let mut writer = Client::connect_with_variant(
        std::net::TcpStream::connect(tcp_proxy_endpoint(&broker)?)?,
        "codex",
        "/",
        65_536,
        Variant::R,
    )
    .map_err(|error| stage_error("connect writer", error))?;
    writer
        .write_file("/specimen.txt", b"changed on compute\n")
        .map_err(|error| stage_error("write specimen", error))?;
    drop(writer);
    assert_eq!(
        fs::read(root.path.join("specimen.txt"))?,
        b"changed on compute\n"
    );
    Ok(())
}

#[test]
fn reverse_export_serves_an_application_owned_tree() -> Result<(), Box<dyn std::error::Error>> {
    let server = generate_key_pair()?;
    let client = generate_key_pair()?;
    let broker = ReverseBroker::start(BrokerConfig {
        reverse_bind: address(Ipv4Addr::LOCALHOST, 0),
        proxy_bind: ProxyEndpoint::tcp(address(Ipv4Addr::LOCALHOST, 0)),
        auth: ServerConfig::new(
            "r9p-reverse-tree-test",
            server.private,
            [(client.public, "participant".to_string())],
        )?,
        peer_principal: "participant".to_string(),
        max_waiting_streams: 2,
        authentication_timeout: Duration::from_secs(2),
        proxy_wait_timeout: Duration::from_secs(2),
    })?;
    let export = ReverseExport::start_handler(
        ReverseExportConfig {
            broker_endpoint: broker.reverse_endpoint(),
            auth: ClientConfig::new("r9p-reverse-tree-test", client.private, server.public)?,
            principal: "participant".to_string(),
            connection_pool: 2,
            connect_timeout: Duration::from_secs(2),
            authentication_timeout: Duration::from_secs(2),
            reconnect_min_delay: Duration::from_millis(25),
            reconnect_max_delay: Duration::from_millis(200),
            server: R9pServerConfig {
                default_msize: 65_536,
                max_msize: 65_536,
                max_fids: 64,
                variant: Variant::Plain,
                ..R9pServerConfig::default()
            },
        },
        || Ok(IdentityHandler),
    )?;
    wait_generic_ready(&broker, &export)?;

    let mut reader = Client::connect(
        std::net::TcpStream::connect(tcp_proxy_endpoint(&broker)?)?,
        "agent",
        "/",
        65_536,
    )?;
    assert_eq!(reader.read_path("/identity")?, b"application-tree\n");
    Ok(())
}

#[test]
fn reverse_export_authenticates_the_end_service_peer() -> Result<(), Box<dyn std::error::Error>> {
    let broker_key = generate_key_pair()?;
    let exporter_key = generate_key_pair()?;
    let service_key = generate_key_pair()?;
    let compute_key = generate_key_pair()?;
    let intruder_key = generate_key_pair()?;
    let broker = ReverseBroker::start(BrokerConfig {
        reverse_bind: address(Ipv4Addr::LOCALHOST, 0),
        proxy_bind: ProxyEndpoint::tcp(address(Ipv4Addr::LOCALHOST, 0)),
        auth: ServerConfig::new(
            "r9p-reverse-placement-test",
            broker_key.private.clone(),
            [(exporter_key.public, "profile-host".to_string())],
        )?,
        peer_principal: "profile-host".to_string(),
        max_waiting_streams: 2,
        authentication_timeout: Duration::from_secs(2),
        proxy_wait_timeout: Duration::from_secs(2),
    })?;
    let export = ReverseExport::start_authenticated_handler(
        ReverseExportConfig {
            broker_endpoint: broker.reverse_endpoint(),
            auth: ClientConfig::new(
                "r9p-reverse-placement-test",
                exporter_key.private,
                broker_key.public,
            )?,
            principal: "profile-host".to_string(),
            connection_pool: 2,
            connect_timeout: Duration::from_secs(2),
            authentication_timeout: Duration::from_secs(2),
            reconnect_min_delay: Duration::from_millis(25),
            reconnect_max_delay: Duration::from_millis(200),
            server: R9pServerConfig {
                default_msize: 65_536,
                max_msize: 65_536,
                max_fids: 64,
                variant: Variant::R,
                ..R9pServerConfig::default()
            },
        },
        ServerConfig::new(
            "agents-profile-service",
            service_key.private,
            [(compute_key.public, "/srv/agents/compute/m7".to_string())],
        )?,
        Duration::from_secs(2),
        || Ok(IdentityHandler),
    )?;
    wait_generic_ready(&broker, &export)?;

    let unauthorized = authenticate_client(
        std::net::TcpStream::connect(tcp_proxy_endpoint(&broker)?)?,
        &ClientConfig::new(
            "agents-profile-service",
            intruder_key.private,
            service_key.public,
        )?,
        "/srv/agents/compute/m7",
        Duration::from_secs(2),
    );
    assert!(unauthorized.is_err());
    wait_for_waiting_stream(&broker)?;

    let stream = authenticate_client(
        std::net::TcpStream::connect(tcp_proxy_endpoint(&broker)?)?,
        &ClientConfig::new(
            "agents-profile-service",
            compute_key.private,
            service_key.public,
        )?,
        "/srv/agents/compute/m7",
        Duration::from_secs(2),
    )?;
    let mut reader =
        Client::connect_with_variant(stream, "/srv/agents/compute/m7", "/", 65_536, Variant::R)?;
    assert_eq!(reader.read_path("/identity")?, b"application-tree\n");
    Ok(())
}

#[cfg(unix)]
#[test]
fn reverse_broker_exposes_a_unix_proxy_endpoint() -> Result<(), Box<dyn std::error::Error>> {
    let server = generate_key_pair()?;
    let client = generate_key_pair()?;
    let runtime = TestRoot::new()?;
    let socket = runtime.path.join("proxy.sock");
    let broker = ReverseBroker::start(BrokerConfig {
        reverse_bind: address(Ipv4Addr::LOCALHOST, 0),
        proxy_bind: ProxyEndpoint::unix(&socket),
        auth: ServerConfig::new(
            "r9p-reverse-unix-test",
            server.private,
            [(client.public, "participant".to_string())],
        )?,
        peer_principal: "participant".to_string(),
        max_waiting_streams: 2,
        authentication_timeout: Duration::from_secs(2),
        proxy_wait_timeout: Duration::from_secs(2),
    })?;
    let export = ReverseExport::start_handler(
        ReverseExportConfig {
            broker_endpoint: broker.reverse_endpoint(),
            auth: ClientConfig::new("r9p-reverse-unix-test", client.private, server.public)?,
            principal: "participant".to_string(),
            connection_pool: 2,
            connect_timeout: Duration::from_secs(2),
            authentication_timeout: Duration::from_secs(2),
            reconnect_min_delay: Duration::from_millis(25),
            reconnect_max_delay: Duration::from_millis(200),
            server: R9pServerConfig {
                default_msize: 65_536,
                max_msize: 65_536,
                max_fids: 64,
                variant: Variant::Plain,
                ..R9pServerConfig::default()
            },
        },
        || Ok(IdentityHandler),
    )?;
    wait_generic_ready(&broker, &export)?;

    assert_eq!(broker.proxy_endpoint(), &ProxyEndpoint::unix(&socket));
    let mut reader = Client::connect(
        std::os::unix::net::UnixStream::connect(&socket)?,
        "agent",
        "/",
        65_536,
    )?;
    assert_eq!(reader.read_path("/identity")?, b"application-tree\n");
    Ok(())
}

#[test]
fn reverse_broker_discards_closed_idle_streams() -> Result<(), Box<dyn std::error::Error>> {
    let server = generate_key_pair()?;
    let client = generate_key_pair()?;
    let server_config = ServerConfig::new(
        "r9p-reverse-stale-test",
        server.private.clone(),
        [(client.public, "laptop".to_string())],
    )?;
    let client_config = ClientConfig::new("r9p-reverse-stale-test", client.private, server.public)?;
    let broker = ReverseBroker::start(BrokerConfig {
        reverse_bind: address(Ipv4Addr::LOCALHOST, 0),
        proxy_bind: ProxyEndpoint::tcp(address(Ipv4Addr::LOCALHOST, 0)),
        auth: server_config,
        peer_principal: "laptop".to_string(),
        max_waiting_streams: 2,
        authentication_timeout: Duration::from_secs(2),
        proxy_wait_timeout: Duration::from_secs(2),
    })?;
    let stale_root = TestRoot::new()?;
    fs::write(stale_root.path.join("identity"), b"stale\n")?;
    let stale = FilesystemExport::start(export_config(
        broker.reverse_endpoint(),
        client_config.clone(),
        stale_root.path.clone(),
        2,
    ))?;
    wait_ready(&broker, &stale)?;
    drop(stale);

    let live_root = TestRoot::new()?;
    fs::write(live_root.path.join("identity"), b"live\n")?;
    let _live = FilesystemExport::start(export_config(
        broker.reverse_endpoint(),
        client_config,
        live_root.path.clone(),
        2,
    ))?;
    let mut reader = Client::connect_with_variant(
        std::net::TcpStream::connect(tcp_proxy_endpoint(&broker)?)?,
        "codex",
        "/",
        65_536,
        Variant::R,
    )?;
    assert_eq!(reader.read_path("/identity")?, b"live\n");
    Ok(())
}

fn export_config(
    broker_endpoint: SocketAddr,
    auth: ClientConfig,
    root: PathBuf,
    connection_pool: usize,
) -> FilesystemExportConfig {
    FilesystemExportConfig {
        broker_endpoint,
        auth,
        principal: "laptop".to_string(),
        root,
        writable: true,
        connection_pool,
        connect_timeout: Duration::from_secs(2),
        authentication_timeout: Duration::from_secs(2),
        reconnect_min_delay: Duration::from_millis(25),
        reconnect_max_delay: Duration::from_millis(200),
        msize: 65_536,
        max_fids: 256,
    }
}

fn stage_error(stage: &str, error: r9p::error::Error) -> Box<dyn std::error::Error> {
    format!("{stage}: {}", String::from_utf8_lossy(error.message())).into()
}

fn wait_ready(
    broker: &ReverseBroker,
    export: &FilesystemExport,
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if broker.is_ready() && export.connected_streams() > 0 {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(10));
    }
    Err("reverse export did not become ready".into())
}

fn wait_generic_ready(
    broker: &ReverseBroker,
    export: &ReverseExport,
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if broker.is_ready() && export.connected_streams() > 0 {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(10));
    }
    Err("generic reverse export did not become ready".into())
}

fn wait_for_waiting_stream(broker: &ReverseBroker) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if broker.is_ready() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(10));
    }
    Err("reverse export did not replenish its pool".into())
}

fn address(ip: Ipv4Addr, port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(ip), port)
}

fn tcp_proxy_endpoint(broker: &ReverseBroker) -> Result<SocketAddr, Box<dyn std::error::Error>> {
    broker
        .proxy_endpoint()
        .as_tcp()
        .ok_or_else(|| "test broker did not expose a TCP proxy".into())
}

struct TestRoot {
    path: PathBuf,
}

impl TestRoot {
    fn new() -> std::io::Result<Self> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("r9p-reverse-test-{}-{nonce}", std::process::id()));
        fs::create_dir(&path)?;
        Ok(Self { path })
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct IdentityHandler;

impl ConnectionHandler for IdentityHandler {
    fn perform(
        &self,
        request: &ServerRequest,
        _cancel: Option<&AtomicBool>,
    ) -> r9p::Result<ServerCompletion> {
        let root = Qid::dir(1);
        let identity = Qid::file(2);
        match &request.kind {
            ServerRequestKind::Attach { .. } => Ok(ServerCompletion::Attach { qid: root }),
            ServerRequestKind::Walk { start, wnames, .. } => {
                let mut current = *start;
                let mut qids = Vec::new();
                for name in wnames {
                    current = match (current.path, name.as_slice()) {
                        (_, b".") => current,
                        (_, b"..") => root,
                        (1, b"identity") => identity,
                        _ => break,
                    };
                    qids.push(current);
                }
                Ok(ServerCompletion::Walk { qids })
            }
            ServerRequestKind::Open { qid, .. } => Ok(ServerCompletion::Open(OpenFile {
                qid: *qid,
                iounit: 0,
            })),
            ServerRequestKind::Read {
                qid, offset, count, ..
            } if *qid == identity => {
                let bytes = b"application-tree\n";
                let start = usize::try_from(*offset)
                    .unwrap_or(usize::MAX)
                    .min(bytes.len());
                let end = start
                    .saturating_add(usize::try_from(*count).unwrap_or(usize::MAX))
                    .min(bytes.len());
                Ok(ServerCompletion::Read(ReadData::Bytes(
                    bytes[start..end].to_vec(),
                )))
            }
            ServerRequestKind::Clunk { .. } => Ok(ServerCompletion::Clunk),
            ServerRequestKind::Stat { qid, .. } if *qid == root => Ok(ServerCompletion::Stat {
                stat: Stat::new(".", *qid, r9p::qid::DMDIR | 0o500),
            }),
            ServerRequestKind::Stat { qid, .. } if *qid == identity => Ok(ServerCompletion::Stat {
                stat: Stat::new("identity", *qid, 0o400),
            }),
            _ => Err(r9p::Error::from_static(EPERM)),
        }
    }
}
