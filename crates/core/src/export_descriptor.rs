use std::{collections::BTreeMap, net::SocketAddr};

use crate::{Error, Result};

pub const EXPORT_FORMAT_V1: &str = "r9p-export.v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportDescriptor {
    pub endpoint_bind: String,
    pub aname: String,
    pub uname: String,
    pub exported_root: String,
    pub transport_class: TransportClass,
    pub mode: ExportMode,
    pub auth: AuthBoundary,
    pub pid: u32,
    pub protocol: Protocol,
    pub msize: u32,
    pub expires_at: Option<String>,
    pub local_root_label: Option<String>,
    pub extra_fields: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportClass {
    Tcp,
    Unix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportMode {
    ReadOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    NineP2000,
    NineP2000L,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthBoundary {
    pub class: AuthClass,
    pub details: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthClass {
    None,
    WireGuard,
    Tailscale,
    UnixPeerCred,
}

impl ExportDescriptor {
    pub fn render(&self) -> Result<String> {
        let mut fields = vec![
            ("format", EXPORT_FORMAT_V1.to_string()),
            ("endpoint_bind", self.endpoint_bind.clone()),
            ("aname", self.aname.clone()),
            ("uname", self.uname.clone()),
            ("exported_root", self.exported_root.clone()),
            ("transport_class", self.transport_class.as_str().to_string()),
            ("mode", self.mode.as_str().to_string()),
            ("auth", self.auth.render()),
            ("pid", self.pid.to_string()),
            ("protocol", self.protocol.as_str().to_string()),
            ("msize", self.msize.to_string()),
        ];
        if let Some(expires_at) = &self.expires_at {
            fields.push(("expires_at", expires_at.clone()));
        }
        if let Some(label) = &self.local_root_label {
            fields.push(("local_root_label", label.clone()));
        }
        for (field, value) in &self.extra_fields {
            validate_extension_field_name(field)?;
            if is_reserved_field(field) {
                return Err(Error::from(format!(
                    "descriptor extension field {field} is reserved"
                )));
            }
            fields.push((field, value.clone()));
        }

        let mut out = String::new();
        for (field, value) in fields {
            validate_token(field, field)?;
            validate_token(field, &value)?;
            out.push_str(field);
            out.push('\t');
            out.push_str(&value);
            out.push('\n');
        }
        Ok(out)
    }

    pub fn parse(input: &str) -> Result<Self> {
        let mut fields = BTreeMap::new();
        let mut extra_fields = BTreeMap::new();
        for (index, line) in input.lines().enumerate() {
            if line.is_empty() {
                continue;
            }
            let parts = line.split('\t').collect::<Vec<_>>();
            if parts.len() != 2 {
                return Err(Error::from(format!(
                    "descriptor line {} is not field-tab-value",
                    index + 1
                )));
            }
            let field = parts[0];
            let value = parts[1];
            validate_token(field, field)?;
            validate_token(field, value)?;
            let target = if is_reserved_field(field) {
                &mut fields
            } else {
                validate_extension_field_name(field)?;
                &mut extra_fields
            };
            if target
                .insert(field.to_string(), value.to_string())
                .is_some()
            {
                return Err(Error::from(format!("duplicate descriptor field {field}")));
            }
        }

        let format = required(&fields, "format")?;
        if format != EXPORT_FORMAT_V1 {
            return Err(Error::from(format!("unknown descriptor format {format}")));
        }

        let descriptor = Self {
            endpoint_bind: required(&fields, "endpoint_bind")?.to_string(),
            aname: required(&fields, "aname")?.to_string(),
            uname: required(&fields, "uname")?.to_string(),
            exported_root: required(&fields, "exported_root")?.to_string(),
            transport_class: TransportClass::parse(required(&fields, "transport_class")?)?,
            mode: ExportMode::parse(required(&fields, "mode")?)?,
            auth: AuthBoundary::parse(required(&fields, "auth")?)?,
            pid: parse_u32(required(&fields, "pid")?, "pid")?,
            protocol: Protocol::parse(required(&fields, "protocol")?)?,
            msize: parse_u32(required(&fields, "msize")?, "msize")?,
            expires_at: fields.get("expires_at").cloned(),
            local_root_label: fields.get("local_root_label").cloned(),
            extra_fields,
        };
        descriptor.validate_authority_boundary()?;
        Ok(descriptor)
    }

    pub fn vault_transport_class(&self) -> Result<String> {
        match self.transport_class {
            TransportClass::Unix => Ok("unix_socket".to_string()),
            TransportClass::Tcp if tcp_endpoint_is_loopback(&self.endpoint_bind) => {
                Ok("loopback".to_string())
            }
            TransportClass::Tcp => match self.auth.class {
                AuthClass::WireGuard | AuthClass::Tailscale if !self.auth.details.is_empty() => {
                    Ok(format!("network_class:{}", self.auth.details))
                }
                _ => Err(Error::from(format!(
                    "descriptor tcp auth boundary not mountable: {}",
                    self.auth.render()
                ))),
            },
        }
    }

    fn validate_authority_boundary(&self) -> Result<()> {
        match (self.transport_class, self.auth.class) {
            (TransportClass::Tcp, AuthClass::None)
                if !tcp_endpoint_is_loopback(&self.endpoint_bind) =>
            {
                return Err(Error::from(
                    "descriptor auth=none is only admitted for loopback TCP",
                ));
            }
            (TransportClass::Tcp, AuthClass::UnixPeerCred) => {
                return Err(Error::from(
                    "descriptor uds-peercred auth is not valid for TCP",
                ));
            }
            (TransportClass::Unix, AuthClass::WireGuard | AuthClass::Tailscale) => {
                return Err(Error::from(
                    "descriptor network auth boundaries are not valid for unix sockets",
                ));
            }
            _ => {}
        }
        Ok(())
    }
}

impl TransportClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Unix => "unix",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "tcp" => Ok(Self::Tcp),
            "unix" => Ok(Self::Unix),
            _ => Err(Error::from(format!("unknown transport_class {value}"))),
        }
    }
}

impl ExportMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "ro",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "ro" => Ok(Self::ReadOnly),
            _ => Err(Error::from(format!("unknown mode {value}"))),
        }
    }
}

impl Protocol {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NineP2000 => "9P2000",
            Self::NineP2000L => "9P2000.L",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "9P2000" => Ok(Self::NineP2000),
            "9P2000.L" => Ok(Self::NineP2000L),
            _ => Err(Error::from(format!("unknown protocol {value}"))),
        }
    }
}

impl AuthBoundary {
    pub fn none() -> Self {
        Self {
            class: AuthClass::None,
            details: String::new(),
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        if value == "none" {
            return Ok(Self::none());
        }
        let (class, details) = value
            .split_once(':')
            .ok_or_else(|| Error::from(format!("invalid auth boundary {value}")))?;
        let class = AuthClass::parse(class)?;
        if class == AuthClass::None || details.is_empty() {
            return Err(Error::from(format!("invalid auth boundary {value}")));
        }
        Ok(Self {
            class,
            details: details.to_string(),
        })
    }

    pub fn render(&self) -> String {
        match self.class {
            AuthClass::None if self.details.is_empty() => "none".to_string(),
            _ => format!("{}:{}", self.class.as_str(), self.details),
        }
    }
}

impl AuthClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::WireGuard => "wg",
            Self::Tailscale => "tailscale",
            Self::UnixPeerCred => "uds-peercred",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "none" => Ok(Self::None),
            "wg" => Ok(Self::WireGuard),
            "tailscale" => Ok(Self::Tailscale),
            "uds-peercred" => Ok(Self::UnixPeerCred),
            _ => Err(Error::from(format!("unknown auth class {value}"))),
        }
    }
}

fn required<'a>(fields: &'a BTreeMap<String, String>, field: &str) -> Result<&'a str> {
    fields
        .get(field)
        .map(String::as_str)
        .ok_or_else(|| Error::from(format!("missing descriptor field {field}")))
}

fn parse_u32(value: &str, field: &str) -> Result<u32> {
    value
        .parse::<u32>()
        .map_err(|_| Error::from(format!("invalid {field} {value}")))
}

fn validate_token(field: &str, value: &str) -> Result<()> {
    if value.contains('\t') || value.contains('\n') || value.contains('\r') {
        return Err(Error::from(format!(
            "descriptor field {field} contains tab or newline"
        )));
    }
    Ok(())
}

fn validate_extension_field_name(field: &str) -> Result<()> {
    if field.is_empty() {
        return Err(Error::from("descriptor extension field is empty"));
    }
    let mut chars = field.chars();
    let first = chars
        .next()
        .ok_or_else(|| Error::from("descriptor extension field is empty"))?;
    if !first.is_ascii_lowercase() {
        return Err(Error::from(format!(
            "descriptor extension field {field} must start with lowercase ascii"
        )));
    }
    if !chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_') {
        return Err(Error::from(format!(
            "descriptor extension field {field} must use lowercase ascii, digits, or underscore"
        )));
    }
    Ok(())
}

fn is_reserved_field(field: &str) -> bool {
    matches!(
        field,
        "format"
            | "endpoint_bind"
            | "aname"
            | "uname"
            | "exported_root"
            | "transport_class"
            | "mode"
            | "auth"
            | "pid"
            | "protocol"
            | "msize"
            | "expires_at"
            | "local_root_label"
    )
}

fn tcp_endpoint_is_loopback(endpoint: &str) -> bool {
    endpoint.starts_with("127.")
        || endpoint.starts_with("localhost:")
        || endpoint.starts_with("[::1]:")
        || endpoint
            .parse::<SocketAddr>()
            .map(|address| address.ip().is_loopback())
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor() -> ExportDescriptor {
        ExportDescriptor {
            endpoint_bind: "127.0.0.1:1234".to_string(),
            aname: "/".to_string(),
            uname: "codex".to_string(),
            exported_root: "/".to_string(),
            transport_class: TransportClass::Tcp,
            mode: ExportMode::ReadOnly,
            auth: AuthBoundary::none(),
            pid: 42,
            protocol: Protocol::NineP2000,
            msize: 65_536,
            expires_at: None,
            local_root_label: Some("/tmp/candidate".to_string()),
            extra_fields: BTreeMap::new(),
        }
    }

    #[test]
    fn descriptor_round_trips() {
        let rendered = descriptor().render().expect("descriptor should render");
        let parsed = ExportDescriptor::parse(&rendered).expect("descriptor should parse");
        assert_eq!(parsed, descriptor());
    }

    #[test]
    fn descriptor_round_trips_extension_fields() {
        let mut descriptor = descriptor();
        descriptor.extra_fields.insert(
            "git_bundle_path".to_string(),
            "/.vault/source.bundle".to_string(),
        );
        let rendered = descriptor.render().expect("descriptor should render");
        let parsed = ExportDescriptor::parse(&rendered).expect("descriptor should parse");
        assert_eq!(
            parsed
                .extra_fields
                .get("git_bundle_path")
                .map(String::as_str),
            Some("/.vault/source.bundle")
        );
    }

    #[test]
    fn descriptor_rejects_duplicate_fields() {
        let input = "format\tr9p-export.v1\nformat\tr9p-export.v1\n";
        assert!(ExportDescriptor::parse(input).is_err());
    }

    #[test]
    fn descriptor_rejects_missing_fields() {
        let input = "format\tr9p-export.v1\n";
        assert!(ExportDescriptor::parse(input).is_err());
    }

    #[test]
    fn descriptor_rejects_unknown_format_and_values() {
        let mut rendered = descriptor().render().expect("descriptor should render");
        rendered = rendered.replace("format\tr9p-export.v1", "format\tr9p-export.v2");
        assert!(ExportDescriptor::parse(&rendered).is_err());

        let mut rendered = descriptor().render().expect("descriptor should render");
        rendered = rendered.replace("mode\tro", "mode\trw");
        assert!(ExportDescriptor::parse(&rendered).is_err());
    }

    #[test]
    fn descriptor_rejects_tabs_and_newlines_in_values() {
        let mut descriptor = descriptor();
        descriptor.endpoint_bind = "127.0.0.1:1234\tbad".to_string();
        assert!(descriptor.render().is_err());
    }

    #[test]
    fn descriptor_rejects_invalid_extension_field_names() {
        let mut descriptor = descriptor();
        descriptor
            .extra_fields
            .insert("GitBundlePath".to_string(), "/bundle".to_string());
        assert!(descriptor.render().is_err());
    }

    #[test]
    fn descriptor_rejects_auth_none_for_non_loopback_tcp() {
        let mut descriptor = descriptor();
        descriptor.endpoint_bind = "192.0.2.1:564".to_string();
        let rendered = descriptor.render().expect("descriptor should render");
        assert!(ExportDescriptor::parse(&rendered).is_err());
    }

    #[test]
    fn descriptor_accepts_network_auth_for_non_loopback_tcp() {
        let mut descriptor = descriptor();
        descriptor.endpoint_bind = "192.0.2.1:564".to_string();
        descriptor.auth = AuthBoundary::parse("wg:m7-dev-lan").expect("auth should parse");
        let rendered = descriptor.render().expect("descriptor should render");
        let parsed = ExportDescriptor::parse(&rendered).expect("descriptor should parse");
        assert_eq!(parsed.auth.render(), "wg:m7-dev-lan");
        assert_eq!(
            parsed
                .vault_transport_class()
                .expect("transport class should render"),
            "network_class:m7-dev-lan"
        );
    }

    #[test]
    fn descriptor_rejects_transport_incompatible_auth_boundaries() {
        let mut tcp = descriptor();
        tcp.auth = AuthBoundary::parse("uds-peercred:1000:100").expect("auth should parse");
        assert!(ExportDescriptor::parse(&tcp.render().expect("descriptor should render")).is_err());

        let mut unix = descriptor();
        unix.transport_class = TransportClass::Unix;
        unix.endpoint_bind = "unix:/tmp/r9p.sock".to_string();
        unix.auth = AuthBoundary::parse("wg:m7-dev-lan").expect("auth should parse");
        assert!(
            ExportDescriptor::parse(&unix.render().expect("descriptor should render")).is_err()
        );
    }
}
