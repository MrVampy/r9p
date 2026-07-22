//! Authenticated reverse-connect transport for 9P.
//!
//! The filesystem-owning host establishes the network connection outward and
//! then serves ordinary 9P on the authenticated stream.  A broker pairs that
//! stream with a loopback 9P client connection.  The broker does not interpret
//! 9P messages or acquire filesystem authority.

mod broker;
mod export;

pub use broker::{BrokerConfig, ReverseBroker};
pub use export::{FilesystemExport, FilesystemExportConfig};

#[cfg(test)]
mod tests;
