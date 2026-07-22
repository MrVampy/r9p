//! Authenticated reverse-connect transport for 9P.
//!
//! The filesystem-owning host establishes the network connection outward and
//! then serves ordinary 9P on the authenticated stream.  A broker pairs that
//! stream with a loopback 9P client connection.  The broker does not interpret
//! 9P messages or acquire filesystem authority.

mod broker;
mod export;

use std::{io, net::TcpStream};

pub use broker::{BrokerConfig, BrokerStatus, ReverseBroker};
pub use export::{FilesystemExport, FilesystemExportConfig, FilesystemExportStatus};

fn configure_transport_socket(stream: &TcpStream) -> io::Result<()> {
    // Reverse sessions carry latency-sensitive 9P request/response frames.
    // Leaving Nagle enabled compounds delayed acknowledgements across the
    // small, sequential walks and stats used by filesystem clients.
    stream.set_nodelay(true)
}

#[cfg(test)]
mod tests;
