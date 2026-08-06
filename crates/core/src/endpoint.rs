use std::{fmt, net::SocketAddr};

use crate::{Error, Result};

const MAX_HOST_BYTES: usize = 255;

/// A syntactically valid TCP host and port that preserves DNS names until dial.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TcpEndpoint {
    host: String,
    port: u16,
}

impl TcpEndpoint {
    pub fn parse(value: &str) -> Result<Self> {
        if value.is_empty()
            || value.len() > MAX_HOST_BYTES + 8
            || value
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
        {
            return Err(Error::from(format!("invalid TCP endpoint {value}")));
        }

        let (host, port) = if let Some(value) = value.strip_prefix('[') {
            let (host, suffix) = value
                .split_once(']')
                .ok_or_else(|| Error::from(format!("invalid TCP endpoint {value}")))?;
            let port = suffix
                .strip_prefix(':')
                .ok_or_else(|| Error::from(format!("invalid TCP endpoint {value}")))?;
            (host, port)
        } else {
            let (host, port) = value
                .rsplit_once(':')
                .ok_or_else(|| Error::from(format!("invalid TCP endpoint {value}")))?;
            if host.contains(':') {
                return Err(Error::from(format!(
                    "IPv6 TCP endpoint must be bracketed: {value}"
                )));
            }
            (host, port)
        };

        if host.is_empty()
            || host.len() > MAX_HOST_BYTES
            || host.bytes().any(|byte| matches!(byte, b'/' | b'[' | b']'))
        {
            return Err(Error::from(format!("invalid TCP endpoint host {host}")));
        }
        let port = port
            .parse::<u16>()
            .ok()
            .filter(|port| *port > 0)
            .ok_or_else(|| Error::from(format!("invalid TCP endpoint port {port}")))?;
        Ok(Self {
            host: host.to_string(),
            port,
        })
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub const fn port(&self) -> u16 {
        self.port
    }

    pub fn is_unspecified_ip(&self) -> bool {
        self.host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_unspecified())
    }
}

impl From<SocketAddr> for TcpEndpoint {
    fn from(value: SocketAddr) -> Self {
        Self {
            host: value.ip().to_string(),
            port: value.port(),
        }
    }
}

impl fmt::Display for TcpEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.host.contains(':') {
            write!(formatter, "[{}]:{}", self.host, self.port)
        } else {
            write!(formatter, "{}:{}", self.host, self.port)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_dns_names_and_ports() -> Result<()> {
        let endpoint = TcpEndpoint::parse("m7.mesh:9564")?;
        assert_eq!(endpoint.host(), "m7.mesh");
        assert_eq!(endpoint.port(), 9564);
        assert_eq!(endpoint.to_string(), "m7.mesh:9564");
        Ok(())
    }

    #[test]
    fn canonicalizes_bracketed_ipv6() -> Result<()> {
        let endpoint = TcpEndpoint::parse("[2001:db8::1]:564")?;
        assert_eq!(endpoint.host(), "2001:db8::1");
        assert_eq!(endpoint.to_string(), "[2001:db8::1]:564");
        Ok(())
    }

    #[test]
    fn rejects_missing_zero_and_ambiguous_ports() {
        for value in ["m7.mesh", "m7.mesh:0", "2001:db8::1:564", ":564"] {
            assert!(TcpEndpoint::parse(value).is_err(), "accepted {value}");
        }
    }
}
