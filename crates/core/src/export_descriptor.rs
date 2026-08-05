use std::{collections::BTreeMap, net::SocketAddr};

use crate::{Error, Result};

pub const EXPORT_FORMAT_V1: &str = "r9p-export.v1";
pub const P9ANY_NOISE_IK: &str = "noise-ik";
/// Mutual-certificate variant. The responder transmits its static key during
/// the handshake instead of the initiator pinning it in advance, so a client
/// needs no per-service key material.
pub const P9ANY_NOISE_XX: &str = "noise-xx";
pub const SESSION_ENDPOINT_BIND_FIELD: &str = "session_endpoint_bind";
pub const SESSION_ANAME_FIELD: &str = "session_aname";
pub const SESSION_AUTH_FIELD: &str = "session_auth";

const MAX_AUTH_DOMAIN_BYTES: usize = 255;

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
    pub namespace_mount_paths: Vec<String>,
    pub extra_fields: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEndpoint {
    pub endpoint_bind: String,
    pub aname: String,
    pub transport_class: TransportClass,
    pub auth: AuthBoundary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportClass {
    Tcp,
    Unix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportMode {
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(non_camel_case_types)]
pub enum Protocol {
    _9P2000,
    _9P2000R,
    _9P2000L,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthBoundary {
    pub class: AuthClass,
    pub details: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthClass {
    None,
    P9any,
    UnixPeerCred,
}

impl ExportDescriptor {
    pub fn with_session_endpoint(mut self, endpoint: SessionEndpoint) -> Result<Self> {
        endpoint.validate()?;
        for (field, value) in [
            (SESSION_ENDPOINT_BIND_FIELD, endpoint.endpoint_bind),
            (SESSION_ANAME_FIELD, endpoint.aname),
            (SESSION_AUTH_FIELD, endpoint.auth.render()),
        ] {
            if self.extra_fields.insert(field.to_string(), value).is_some() {
                return Err(Error::from(format!(
                    "descriptor already contains session endpoint field {field}"
                )));
            }
        }
        Ok(self)
    }

    pub fn session_endpoint(&self) -> Result<Option<SessionEndpoint>> {
        let bind = self.extra_fields.get(SESSION_ENDPOINT_BIND_FIELD);
        let aname = self.extra_fields.get(SESSION_ANAME_FIELD);
        let auth = self.extra_fields.get(SESSION_AUTH_FIELD);
        match (bind, aname, auth) {
            (None, None, None) => Ok(None),
            (Some(bind), Some(aname), Some(auth)) => {
                let endpoint = SessionEndpoint {
                    endpoint_bind: bind.clone(),
                    aname: aname.clone(),
                    transport_class: transport_class_for_endpoint(bind),
                    auth: AuthBoundary::parse(auth)?,
                };
                endpoint.validate()?;
                Ok(Some(endpoint))
            }
            _ => Err(Error::from("descriptor has an incomplete session endpoint")),
        }
    }

    pub fn render(&self) -> Result<String> {
        self.validate_authority_boundary()?;
        self.session_endpoint()?;
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
        if !self.namespace_mount_paths.is_empty() {
            fields.push((
                "namespace_mount_paths",
                self.namespace_mount_paths.join(","),
            ));
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
            namespace_mount_paths: parse_namespace_mount_paths(
                fields.get("namespace_mount_paths"),
            )?,
            extra_fields,
        };
        descriptor.validate_authority_boundary()?;
        descriptor.session_endpoint()?;
        Ok(descriptor)
    }

    fn validate_authority_boundary(&self) -> Result<()> {
        validate_transport_auth(self.transport_class, &self.endpoint_bind, &self.auth)
    }
}

impl SessionEndpoint {
    fn validate(&self) -> Result<()> {
        validate_token(SESSION_ENDPOINT_BIND_FIELD, &self.endpoint_bind)?;
        validate_token(SESSION_ANAME_FIELD, &self.aname)?;
        if self.endpoint_bind.is_empty() {
            return Err(Error::from("session endpoint bind is empty"));
        }
        if self.aname.is_empty() {
            return Err(Error::from("session endpoint aname is empty"));
        }
        validate_transport_auth(self.transport_class, &self.endpoint_bind, &self.auth)
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
            Self::ReadWrite => "rw",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "ro" => Ok(Self::ReadOnly),
            "rw" => Ok(Self::ReadWrite),
            _ => Err(Error::from(format!("unknown mode {value}"))),
        }
    }
}

impl Protocol {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::_9P2000 => "9P2000",
            Self::_9P2000R => "9P2000.R",
            Self::_9P2000L => "9P2000.L",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "9P2000" => Ok(Self::_9P2000),
            "9P2000.R" => Ok(Self::_9P2000R),
            "9P2000.L" => Ok(Self::_9P2000L),
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
        let boundary = Self {
            class,
            details: details.to_string(),
        };
        boundary.validate()?;
        Ok(boundary)
    }

    pub fn p9any_noise_ik(domain: &str) -> Result<Self> {
        validate_p9any_domain(domain)?;
        Ok(Self {
            class: AuthClass::P9any,
            details: format!("{P9ANY_NOISE_IK}@{domain}"),
        })
    }

    pub fn p9any_noise_xx(domain: &str) -> Result<Self> {
        validate_p9any_domain(domain)?;
        Ok(Self {
            class: AuthClass::P9any,
            details: format!("{P9ANY_NOISE_XX}@{domain}"),
        })
    }

    /// The service name a caller must see proved before it trusts the session.
    /// Under XX this is checked against the responder's certificate, which is
    /// what makes a referral safe to take from an addressing service: it can
    /// point you somewhere, it cannot change who answers.
    pub fn p9any_domain(&self) -> Option<&str> {
        if self.class != AuthClass::P9any {
            return None;
        }
        self.details
            .strip_prefix(P9ANY_NOISE_XX)
            .or_else(|| self.details.strip_prefix(P9ANY_NOISE_IK))
            .and_then(|value| value.strip_prefix('@'))
    }

    pub fn render(&self) -> String {
        match self.class {
            AuthClass::None if self.details.is_empty() => "none".to_string(),
            _ => format!("{}:{}", self.class.as_str(), self.details),
        }
    }

    fn validate(&self) -> Result<()> {
        match self.class {
            AuthClass::None if self.details.is_empty() => Ok(()),
            AuthClass::P9any => {
                let domain = self.p9any_domain().ok_or_else(|| {
                    Error::from(format!(
                        "p9any auth boundary must use {P9ANY_NOISE_IK}@domain or {P9ANY_NOISE_XX}@domain"
                    ))
                })?;
                validate_p9any_domain(domain)
            }
            AuthClass::UnixPeerCred if !self.details.is_empty() => Ok(()),
            _ => Err(Error::from(format!(
                "invalid auth boundary {}",
                self.render()
            ))),
        }
    }
}

impl AuthClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::P9any => "p9any",
            Self::UnixPeerCred => "uds-peercred",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "none" => Ok(Self::None),
            "p9any" => Ok(Self::P9any),
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

pub fn validate_p9any_domain(domain: &str) -> Result<()> {
    if domain.is_empty() || domain.len() > MAX_AUTH_DOMAIN_BYTES {
        return Err(Error::from(format!(
            "auth domain must contain 1 to {MAX_AUTH_DOMAIN_BYTES} bytes"
        )));
    }
    if !domain
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(Error::from(
            "auth domain must use ascii letters, digits, dot, dash, or underscore",
        ));
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
            | "namespace_mount_paths"
    )
}

fn parse_namespace_mount_paths(value: Option<&String>) -> Result<Vec<String>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    trimmed
        .split(',')
        .map(|path| {
            let path = path.trim();
            validate_namespace_mount_path(path)?;
            Ok(path.to_string())
        })
        .collect()
}

fn validate_namespace_mount_path(path: &str) -> Result<()> {
    if path.is_empty() {
        return Err(Error::from("empty namespace_mount_paths entry"));
    }
    if !path.starts_with('/') {
        return Err(Error::from(format!(
            "namespace_mount_paths entry is not absolute: {path}"
        )));
    }
    if path == "/" {
        return Err(Error::from("namespace_mount_paths entry cannot be root"));
    }
    Ok(())
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

fn transport_class_for_endpoint(endpoint: &str) -> TransportClass {
    if endpoint.starts_with("unix:") || endpoint.starts_with("unix!") {
        TransportClass::Unix
    } else {
        TransportClass::Tcp
    }
}

fn validate_transport_auth(
    transport_class: TransportClass,
    endpoint_bind: &str,
    auth: &AuthBoundary,
) -> Result<()> {
    auth.validate()?;
    match (transport_class, auth.class) {
        (TransportClass::Tcp, AuthClass::None) if !tcp_endpoint_is_loopback(endpoint_bind) => Err(
            Error::from("descriptor auth=none is only admitted for loopback TCP"),
        ),
        (TransportClass::Tcp, AuthClass::UnixPeerCred) => Err(Error::from(
            "descriptor uds-peercred auth is not valid for TCP",
        )),
        (TransportClass::Unix, AuthClass::P9any) => Err(Error::from(
            "descriptor p9any session auth is not valid for unix sockets",
        )),
        _ => Ok(()),
    }
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
            protocol: Protocol::_9P2000,
            msize: 65_536,
            expires_at: None,
            local_root_label: Some("/tmp/candidate".to_string()),
            namespace_mount_paths: Vec::new(),
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
            "content_path".to_string(),
            "/export/content.bin".to_string(),
        );
        let rendered = descriptor.render().expect("descriptor should render");
        let parsed = ExportDescriptor::parse(&rendered).expect("descriptor should parse");
        assert_eq!(
            parsed.extra_fields.get("content_path").map(String::as_str),
            Some("/export/content.bin")
        );
    }

    #[test]
    fn descriptor_round_trips_authenticated_session_endpoint() {
        let expected = SessionEndpoint {
            endpoint_bind: "192.0.2.10:19640".to_string(),
            aname: "service-generation".to_string(),
            transport_class: TransportClass::Tcp,
            auth: AuthBoundary::p9any_noise_ik("agents").expect("auth should be valid"),
        };
        let descriptor = descriptor()
            .with_session_endpoint(expected.clone())
            .expect("session endpoint should be valid");
        let rendered = descriptor.render().expect("descriptor should render");
        let parsed = ExportDescriptor::parse(&rendered).expect("descriptor should parse");
        assert_eq!(
            parsed
                .session_endpoint()
                .expect("session endpoint should parse"),
            Some(expected)
        );
    }

    #[test]
    fn descriptor_rejects_incomplete_session_endpoint() {
        let mut descriptor = descriptor();
        descriptor.extra_fields.insert(
            SESSION_ENDPOINT_BIND_FIELD.to_string(),
            "192.0.2.10:19640".to_string(),
        );
        assert!(descriptor.render().is_err());
    }

    #[test]
    fn descriptor_round_trips_namespace_mount_paths() {
        let mut descriptor = descriptor();
        descriptor.namespace_mount_paths = vec![
            "/sensors/polymarket".to_string(),
            "/markets/polymarket".to_string(),
        ];
        let rendered = descriptor.render().expect("descriptor should render");
        assert!(
            rendered.contains("namespace_mount_paths\t/sensors/polymarket,/markets/polymarket\n")
        );
        let parsed = ExportDescriptor::parse(&rendered).expect("descriptor should parse");
        assert_eq!(
            parsed.namespace_mount_paths,
            descriptor.namespace_mount_paths
        );
    }

    #[test]
    fn descriptor_round_trips_read_write_mode() {
        let mut descriptor = descriptor();
        descriptor.mode = ExportMode::ReadWrite;
        let rendered = descriptor.render().expect("descriptor should render");
        assert!(rendered.contains("mode\trw\n"));
        let parsed = ExportDescriptor::parse(&rendered).expect("descriptor should parse");
        assert_eq!(parsed.mode, ExportMode::ReadWrite);
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
        rendered = rendered.replace("mode\tro", "mode\tbad");
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
        assert!(descriptor.render().is_err());
    }

    #[test]
    fn descriptor_accepts_network_auth_for_non_loopback_tcp() {
        let mut descriptor = descriptor();
        descriptor.endpoint_bind = "192.0.2.1:564".to_string();
        descriptor.auth = AuthBoundary::parse("p9any:noise-ik@vault").expect("auth should parse");
        let rendered = descriptor.render().expect("descriptor should render");
        let parsed = ExportDescriptor::parse(&rendered).expect("descriptor should parse");
        assert_eq!(parsed.auth.render(), "p9any:noise-ik@vault");
    }

    #[test]
    fn descriptor_rejects_transport_incompatible_auth_boundaries() {
        let mut tcp = descriptor();
        tcp.auth = AuthBoundary::parse("uds-peercred:1000:100").expect("auth should parse");
        assert!(tcp.render().is_err());

        let mut unix = descriptor();
        unix.transport_class = TransportClass::Unix;
        unix.endpoint_bind = "unix:/tmp/r9p.sock".to_string();
        unix.auth = AuthBoundary::parse("p9any:noise-ik@vault").expect("auth should parse");
        assert!(unix.render().is_err());
    }

    #[test]
    fn descriptor_rejects_unknown_p9any_provider_and_invalid_domain() {
        assert!(AuthBoundary::parse("p9any:dp9ik@vault").is_err());
        assert!(AuthBoundary::parse("p9any:noise-ik@vault/domain").is_err());
        assert!(AuthBoundary::parse("p9any:noise-ik@").is_err());
    }
}

#[cfg(test)]
mod xx_boundary_tests {
    use super::*;

    #[test]
    fn an_xx_boundary_round_trips_and_names_the_expected_responder() -> Result<()> {
        let boundary = AuthBoundary::p9any_noise_xx("terminal-m7")?;
        assert_eq!(boundary.render(), "p9any:noise-xx@terminal-m7");
        let parsed = AuthBoundary::parse(&boundary.render())?;
        assert_eq!(parsed.p9any_domain(), Some("terminal-m7"));
        Ok(())
    }

    #[test]
    fn the_ik_boundary_still_parses_while_the_fleet_migrates() -> Result<()> {
        let parsed = AuthBoundary::parse("p9any:noise-ik@terminal-m7")?;
        assert_eq!(parsed.p9any_domain(), Some("terminal-m7"));
        Ok(())
    }

    #[test]
    fn an_unknown_p9any_protocol_is_refused() {
        assert!(AuthBoundary::parse("p9any:noise-zz@terminal-m7").is_err());
    }
}
