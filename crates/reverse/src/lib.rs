//! Authenticated reverse-connect transport for 9P.
//!
//! The filesystem-owning host establishes the network connection outward and
//! then serves ordinary 9P on the authenticated stream. A broker pairs that
//! stream with a local client connection by default. An embedding application
//! may explicitly expose a concrete network listener only when every bridged
//! session has its own end-service authentication boundary. The broker does
//! not interpret 9P messages or acquire filesystem authority.

mod broker;
mod claim;
mod export;
mod session_proxy;

use std::{io, net::TcpStream, time::Duration};

use claim::{receive_session_claim, send_session_claim};
use socket2::{SockRef, TcpKeepalive};

pub use broker::{BrokerConfig, BrokerStatus, ProxyEndpoint, ProxyExposure, ReverseBroker};
pub use export::{
    FilesystemExport, FilesystemExportConfig, ReverseExport, ReverseExportConfig,
    ReverseExportStatus,
};
pub use session_proxy::{SessionProxy, SessionProxyConfig, SessionProxyStatus};

const KEEPALIVE_IDLE: Duration = Duration::from_secs(30);
#[cfg(target_os = "linux")]
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);
#[cfg(target_os = "linux")]
const KEEPALIVE_RETRIES: u32 = 3;

fn configure_transport_socket(stream: &TcpStream) -> io::Result<()> {
    // Reverse sessions carry latency-sensitive 9P request/response frames.
    // Leaving Nagle enabled compounds delayed acknowledgements across the
    // small, sequential walks and stats used by filesystem clients.
    stream.set_nodelay(true)?;

    // Pool streams can remain application-idle indefinitely. Without bounded
    // TCP keepalive, a hard peer outage can leave the old pool apparently
    // connected for the host kernel's multi-hour default and prevent the
    // export loop from reconnecting to a restarted broker.
    let keepalive = TcpKeepalive::new().with_time(KEEPALIVE_IDLE);
    #[cfg(target_os = "linux")]
    let keepalive = keepalive
        .with_interval(KEEPALIVE_INTERVAL)
        .with_retries(KEEPALIVE_RETRIES);
    SockRef::from(stream).set_tcp_keepalive(&keepalive)
}

#[cfg(test)]
mod tests;
