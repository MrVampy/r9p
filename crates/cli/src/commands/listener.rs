use std::net::{SocketAddr, ToSocketAddrs};

use crate::errors::{cli_error, CliResult};

pub(super) fn parse_tcp_bind(value: &str) -> CliResult<SocketAddr> {
    let value = match value.strip_prefix("tcp!") {
        Some(rest) => {
            let parts = rest.split('!').collect::<Vec<_>>();
            if parts.len() != 2 {
                return Err(cli_error(format!("invalid tcp bind address {value}")));
            }
            format!("{}:{}", parts[0], parts[1])
        }
        None => value.to_string(),
    };
    let mut addresses = value
        .to_socket_addrs()
        .map_err(|error| cli_error(format!("invalid tcp bind address {value}: {error}")))?;
    addresses
        .next()
        .ok_or_else(|| cli_error(format!("tcp bind address {value} resolved no addresses")))
}
