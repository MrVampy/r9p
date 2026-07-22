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
        reconnect_delay: Duration::from_millis(25),
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
    )?;
    assert_eq!(reader.read_path("/specimen.txt")?, b"from laptop\n");
    drop(reader);

    wait_for_waiting_stream(&broker)?;
    let mut writer = Client::connect_with_variant(
        std::net::TcpStream::connect(broker.proxy_endpoint())?,
        "codex",
        "/",
        65_536,
        Variant::R9pSymlink,
    )?;
    writer.write_file("/specimen.txt", b"changed on compute\n")?;
    drop(writer);
    assert_eq!(
        fs::read(root.path.join("specimen.txt"))?,
        b"changed on compute\n"
    );
    Ok(())
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
