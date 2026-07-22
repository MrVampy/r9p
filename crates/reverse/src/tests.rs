use std::{
    fs,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use r9p::{blocking::Client, codec::Variant};
use r9p_auth::{generate_key_pair, ClientConfig, ServerConfig};

use super::{BrokerConfig, FilesystemExport, FilesystemExportConfig, ReverseBroker};

#[test]
fn reverse_transport_socket_disables_nagle() -> Result<(), Box<dyn std::error::Error>> {
    let listener = std::net::TcpListener::bind(address(Ipv4Addr::LOCALHOST, 0))?;
    let client = std::net::TcpStream::connect(listener.local_addr()?)?;
    let (server, _) = listener.accept()?;

    super::configure_transport_socket(&client)?;
    super::configure_transport_socket(&server)?;

    assert!(client.nodelay()?);
    assert!(server.nodelay()?);
    Ok(())
}

#[test]
fn reverse_export_serves_a_writable_file_tree() -> Result<(), Box<dyn std::error::Error>> {
    let server = generate_key_pair()?;
    let client = generate_key_pair()?;
    let broker = ReverseBroker::start(BrokerConfig {
        reverse_bind: address(Ipv4Addr::LOCALHOST, 0),
        proxy_bind: address(Ipv4Addr::LOCALHOST, 0),
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
        std::net::TcpStream::connect(broker.proxy_endpoint())?,
        "codex",
        "/",
        65_536,
        Variant::R9pSymlink,
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
        std::net::TcpStream::connect(broker.proxy_endpoint())?,
        "codex",
        "/",
        65_536,
        Variant::R9pSymlink,
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
        proxy_bind: address(Ipv4Addr::LOCALHOST, 0),
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
        std::net::TcpStream::connect(broker.proxy_endpoint())?,
        "codex",
        "/",
        65_536,
        Variant::R9pSymlink,
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
