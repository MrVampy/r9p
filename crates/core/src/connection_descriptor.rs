use std::collections::BTreeMap;

use crate::{Error, Result};

pub const CONNECTION_FORMAT_V1: &str = "r9p-connection.v1";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectionDescriptor {
    pub service: String,
    pub channel_id: String,
    pub endpoint_bind: String,
    pub uname: String,
    pub aname: String,
    pub exported_root: String,
    pub namespace_mount_path: Option<String>,
    pub authority_boundary: String,
    pub generation: u64,
    pub valid_for_ms: u64,
}

impl ConnectionDescriptor {
    pub fn render(&self) -> Result<String> {
        self.validate()?;
        let mut fields = vec![
            ("format", CONNECTION_FORMAT_V1.to_string()),
            ("service", self.service.clone()),
            ("channel_id", self.channel_id.clone()),
            ("endpoint_bind", self.endpoint_bind.clone()),
            ("uname", self.uname.clone()),
            ("aname", self.aname.clone()),
            ("exported_root", self.exported_root.clone()),
        ];
        if let Some(namespace_mount_path) = &self.namespace_mount_path {
            fields.push(("namespace_mount_path", namespace_mount_path.clone()));
        }
        fields.extend([
            ("authority_boundary", self.authority_boundary.clone()),
            ("generation", self.generation.to_string()),
            ("valid_for_ms", self.valid_for_ms.to_string()),
        ]);
        let mut rendered = String::new();
        for (field, value) in fields {
            validate_token(field, &value)?;
            rendered.push_str(field);
            rendered.push('\t');
            rendered.push_str(&value);
            rendered.push('\n');
        }
        Ok(rendered)
    }

    pub fn routed_path(&self, namespace_path: &str) -> Result<String> {
        let mount_path = self.namespace_mount_path.as_deref().ok_or_else(|| {
            Error::from(
                "connection descriptor does not carry a resolved namespace mount path",
            )
        })?;
        validate_absolute_path("namespace_path", namespace_path, true)?;
        let suffix = mounted_suffix(namespace_path, mount_path).ok_or_else(|| {
            Error::from(format!(
                "namespace path {namespace_path} is outside resolved mount {mount_path}"
            ))
        })?;
        Ok(join_namespace_path(&self.exported_root, suffix))
    }

    pub fn parse(input: &str) -> Result<Self> {
        let mut fields = BTreeMap::new();
        for (index, line) in input.lines().enumerate() {
            if line.is_empty() {
                continue;
            }
            let Some((field, value)) = line.split_once('\t') else {
                return Err(Error::from(format!(
                    "connection descriptor line {} is not field-tab-value",
                    index + 1
                )));
            };
            if value.contains('\t') {
                return Err(Error::from(format!(
                    "connection descriptor line {} has more than two fields",
                    index + 1
                )));
            }
            if !known_field(field) {
                return Err(Error::from(format!(
                    "unknown connection descriptor field {field}"
                )));
            }
            validate_token(field, value)?;
            if fields
                .insert(field.to_string(), value.to_string())
                .is_some()
            {
                return Err(Error::from(format!(
                    "duplicate connection descriptor field {field}"
                )));
            }
        }
        let format = required(&fields, "format")?;
        if format != CONNECTION_FORMAT_V1 {
            return Err(Error::from(format!(
                "unknown connection descriptor format {format}"
            )));
        }
        let descriptor = Self {
            service: required(&fields, "service")?.to_string(),
            channel_id: required(&fields, "channel_id")?.to_string(),
            endpoint_bind: required(&fields, "endpoint_bind")?.to_string(),
            uname: required(&fields, "uname")?.to_string(),
            aname: required(&fields, "aname")?.to_string(),
            exported_root: required(&fields, "exported_root")?.to_string(),
            namespace_mount_path: fields.get("namespace_mount_path").cloned(),
            authority_boundary: required(&fields, "authority_boundary")?.to_string(),
            generation: parse_u64(required(&fields, "generation")?, "generation")?,
            valid_for_ms: parse_u64(required(&fields, "valid_for_ms")?, "valid_for_ms")?,
        };
        descriptor.validate()?;
        Ok(descriptor)
    }

    fn validate(&self) -> Result<()> {
        for (field, value) in [
            ("service", self.service.as_str()),
            ("channel_id", self.channel_id.as_str()),
            ("endpoint_bind", self.endpoint_bind.as_str()),
            ("uname", self.uname.as_str()),
            ("aname", self.aname.as_str()),
            ("exported_root", self.exported_root.as_str()),
            ("authority_boundary", self.authority_boundary.as_str()),
        ] {
            validate_nonempty(field, value)?;
            validate_token(field, value)?;
        }
        validate_absolute_path("exported_root", &self.exported_root, true)?;
        if let Some(namespace_mount_path) = &self.namespace_mount_path {
            validate_absolute_path("namespace_mount_path", namespace_mount_path, false)?;
        }
        if self.generation == 0 {
            return Err(Error::from(
                "connection descriptor generation must be positive",
            ));
        }
        if self.valid_for_ms == 0 {
            return Err(Error::from(
                "connection descriptor valid_for_ms must be positive",
            ));
        }
        Ok(())
    }
}

fn known_field(field: &str) -> bool {
    matches!(
        field,
        "format"
            | "service"
            | "channel_id"
            | "endpoint_bind"
            | "uname"
            | "aname"
            | "exported_root"
            | "namespace_mount_path"
            | "authority_boundary"
            | "generation"
            | "valid_for_ms"
    )
}

fn required<'a>(fields: &'a BTreeMap<String, String>, field: &str) -> Result<&'a str> {
    fields
        .get(field)
        .map(String::as_str)
        .ok_or_else(|| Error::from(format!("missing connection descriptor field {field}")))
}

fn parse_u64(value: &str, field: &str) -> Result<u64> {
    value
        .parse::<u64>()
        .map_err(|_| Error::from(format!("invalid connection descriptor {field} {value}")))
}

fn validate_nonempty(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(Error::from(format!(
            "connection descriptor field {field} is empty"
        )));
    }
    Ok(())
}

fn validate_token(field: &str, value: &str) -> Result<()> {
    if value
        .bytes()
        .any(|byte| matches!(byte, b'\t' | b'\n' | b'\r'))
    {
        return Err(Error::from(format!(
            "connection descriptor field {field} contains tab or newline"
        )));
    }
    Ok(())
}

fn validate_absolute_path(field: &str, path: &str, allow_root: bool) -> Result<()> {
    if !path.starts_with('/')
        || (!allow_root && path == "/")
        || (path.len() > 1 && path.ends_with('/'))
        || path
            .split('/')
            .skip(1)
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
    {
        return Err(Error::from(format!(
            "connection descriptor {field} must be a canonical absolute path"
        )));
    }
    Ok(())
}

fn mounted_suffix<'a>(path: &'a str, mount_path: &str) -> Option<&'a str> {
    if path == mount_path {
        return Some("");
    }
    path.strip_prefix(mount_path)
        .filter(|suffix| suffix.starts_with('/'))
}

fn join_namespace_path(root: &str, suffix: &str) -> String {
    match (root, suffix) {
        ("/", "") => "/".to_string(),
        ("/", suffix) => suffix.to_string(),
        (root, "") => root.to_string(),
        (root, suffix) => format!("{root}{suffix}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor() -> ConnectionDescriptor {
        ConnectionDescriptor {
            service: "infra/agents".to_string(),
            channel_id: "r9p-export:infra/agents".to_string(),
            endpoint_bind: "127.0.0.1:19640".to_string(),
            uname: "agents.reader".to_string(),
            aname: "/".to_string(),
            exported_root: "/agents".to_string(),
            namespace_mount_path: None,
            authority_boundary: "loopback".to_string(),
            generation: 4,
            valid_for_ms: 19_000,
        }
    }

    #[test]
    fn connection_descriptor_round_trips() {
        let expected = descriptor();
        let rendered = expected.render().expect("render descriptor");
        let parsed = ConnectionDescriptor::parse(&rendered).expect("parse descriptor");
        assert_eq!(parsed, expected);
    }

    #[test]
    fn connection_descriptor_rejects_unknown_fields() {
        let rendered = descriptor().render().expect("render descriptor");
        assert!(ConnectionDescriptor::parse(&format!("{rendered}posture\treverse\n")).is_err());
    }

    #[test]
    fn connection_descriptor_rejects_nonportable_absolute_validity() {
        let rendered = descriptor()
            .render()
            .expect("render descriptor")
            .replace("valid_for_ms\t19000", "valid_until_ms\t19000");
        assert!(ConnectionDescriptor::parse(&rendered).is_err());
    }

    #[test]
    fn connection_descriptor_requires_absolute_exported_root() {
        let mut descriptor = descriptor();
        descriptor.exported_root = "agents".to_string();
        assert!(descriptor.render().is_err());
    }

    #[test]
    fn resolved_namespace_mount_routes_into_exported_root() {
        let mut descriptor = descriptor();
        descriptor.exported_root = "/".to_string();
        descriptor.namespace_mount_path = Some("/agents".to_string());
        assert_eq!(
            descriptor
                .routed_path("/agents/terminals/t-1/output")
                .expect("path should route"),
            "/terminals/t-1/output"
        );
        assert!(descriptor.routed_path("/agents-extra/status").is_err());
    }

    #[test]
    fn namespace_mount_path_must_be_canonical_and_non_root() {
        for invalid in ["/", "agents", "/agents/", "/agents//status", "/agents/../status"] {
            let mut descriptor = descriptor();
            descriptor.namespace_mount_path = Some(invalid.to_string());
            assert!(
                descriptor.render().is_err(),
                "accepted invalid namespace mount path {invalid:?}"
            );
        }
    }

    #[test]
    fn connection_descriptor_rejects_expired_shape_without_validity() {
        let mut descriptor = descriptor();
        descriptor.valid_for_ms = 0;
        assert!(descriptor.render().is_err());
    }
}
