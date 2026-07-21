use crate::{PrivateKey, PublicKey, CONFIG_FORMAT};
use r9p::error::{Error, Result};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

const MAX_DOMAIN_BYTES: usize = 255;
const MAX_PRINCIPAL_BYTES: usize = 255;

#[derive(Clone, Debug)]
pub struct ClientConfig {
    domain: String,
    private_key: PrivateKey,
    server_key: PublicKey,
}

#[derive(Clone, Debug)]
pub struct ServerConfig {
    domain: String,
    private_key: PrivateKey,
    peers: BTreeMap<PublicKey, BTreeSet<String>>,
}

impl ClientConfig {
    pub fn new(
        domain: impl Into<String>,
        private_key: PrivateKey,
        server_key: PublicKey,
    ) -> Result<Self> {
        let domain = domain.into();
        validate_domain(&domain)?;
        Ok(Self {
            domain,
            private_key,
            server_key,
        })
    }

    pub fn read(path: &Path) -> Result<Self> {
        let parsed = ParsedConfig::read(path)?;
        if parsed.role.as_deref() != Some("client") {
            return Err(Error::from("session auth config role must be client"));
        }
        if !parsed.peers.is_empty() {
            return Err(Error::from(
                "client session auth config cannot contain peer entries",
            ));
        }
        let private_path = resolve_path(path, required(parsed.private_key, "private-key")?);
        Self::new(
            required(parsed.domain, "domain")?,
            PrivateKey::read(&private_path)?,
            PublicKey::from_hex(&required(parsed.server_key, "server-key")?)?,
        )
    }

    pub fn domain(&self) -> &str {
        &self.domain
    }

    pub fn private_key(&self) -> &PrivateKey {
        &self.private_key
    }

    pub const fn server_key(&self) -> PublicKey {
        self.server_key
    }
}

impl ServerConfig {
    pub fn new(
        domain: impl Into<String>,
        private_key: PrivateKey,
        peers: impl IntoIterator<Item = (PublicKey, String)>,
    ) -> Result<Self> {
        let domain = domain.into();
        validate_domain(&domain)?;
        let mut allowed = BTreeMap::<PublicKey, BTreeSet<String>>::new();
        for (key, principal) in peers {
            validate_principal(&principal)?;
            if !allowed.entry(key).or_default().insert(principal.clone()) {
                return Err(Error::from(format!(
                    "duplicate authorized peer principal {principal}"
                )));
            }
        }
        if allowed.is_empty() {
            return Err(Error::from(
                "server session auth config requires at least one peer",
            ));
        }
        Ok(Self {
            domain,
            private_key,
            peers: allowed,
        })
    }

    pub fn read(path: &Path) -> Result<Self> {
        let parsed = ParsedConfig::read(path)?;
        if parsed.role.as_deref() != Some("server") {
            return Err(Error::from("session auth config role must be server"));
        }
        if parsed.server_key.is_some() {
            return Err(Error::from(
                "server session auth config cannot contain server-key",
            ));
        }
        let private_path = resolve_path(path, required(parsed.private_key, "private-key")?);
        let mut peers = Vec::with_capacity(parsed.peers.len());
        for (key, principal) in parsed.peers {
            peers.push((PublicKey::from_hex(&key)?, principal));
        }
        Self::new(
            required(parsed.domain, "domain")?,
            PrivateKey::read(&private_path)?,
            peers,
        )
    }

    pub fn domain(&self) -> &str {
        &self.domain
    }

    pub fn private_key(&self) -> &PrivateKey {
        &self.private_key
    }

    pub fn authorize(&self, key: PublicKey, principal: &str) -> bool {
        self.peers
            .get(&key)
            .is_some_and(|principals| principals.contains(principal))
    }
}

pub(crate) fn validate_principal(principal: &str) -> Result<()> {
    if principal.is_empty() || principal.len() > MAX_PRINCIPAL_BYTES {
        return Err(Error::from(format!(
            "session principal must contain 1 to {MAX_PRINCIPAL_BYTES} bytes"
        )));
    }
    if principal
        .bytes()
        .any(|byte| byte == 0 || byte.is_ascii_control())
    {
        return Err(Error::from("session principal contains a control byte"));
    }
    Ok(())
}

fn validate_domain(domain: &str) -> Result<()> {
    if domain.is_empty() || domain.len() > MAX_DOMAIN_BYTES {
        return Err(Error::from(format!(
            "auth domain must contain 1 to {MAX_DOMAIN_BYTES} bytes"
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

#[derive(Default)]
struct ParsedConfig {
    role: Option<String>,
    domain: Option<String>,
    private_key: Option<String>,
    server_key: Option<String>,
    peers: Vec<(String, String)>,
}

impl ParsedConfig {
    fn read(path: &Path) -> Result<Self> {
        let input = fs::read_to_string(path).map_err(|error| {
            Error::from(format!(
                "read session auth config {}: {error}",
                path.display()
            ))
        })?;
        let mut parsed = Self::default();
        let mut format = None;
        for (index, raw_line) in input.lines().enumerate() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut fields = line.split_ascii_whitespace();
            let name = fields.next().unwrap_or_default();
            let values = fields.collect::<Vec<_>>();
            match (name, values.as_slice()) {
                ("format", [value]) => set_once(&mut format, value, "format")?,
                ("role", [value]) => set_once(&mut parsed.role, value, "role")?,
                ("domain", [value]) => set_once(&mut parsed.domain, value, "domain")?,
                ("private-key", [value]) => {
                    set_once(&mut parsed.private_key, value, "private-key")?
                }
                ("server-key", [value]) => set_once(&mut parsed.server_key, value, "server-key")?,
                ("peer", [key, principal]) => {
                    parsed
                        .peers
                        .push(((*key).to_string(), (*principal).to_string()));
                }
                _ => {
                    return Err(Error::from(format!(
                        "invalid session auth config line {} in {}",
                        index + 1,
                        path.display()
                    )));
                }
            }
        }
        if format.as_deref() != Some(CONFIG_FORMAT) {
            return Err(Error::from(format!(
                "session auth config format must be {CONFIG_FORMAT}"
            )));
        }
        Ok(parsed)
    }
}

fn set_once(target: &mut Option<String>, value: &str, field: &str) -> Result<()> {
    if target.replace(value.to_string()).is_some() {
        Err(Error::from(format!(
            "duplicate session auth config field {field}"
        )))
    } else {
        Ok(())
    }
}

fn required(value: Option<String>, field: &str) -> Result<String> {
    value.ok_or_else(|| Error::from(format!("missing session auth config field {field}")))
}

fn resolve_path(config_path: &Path, value: String) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_authorization_is_key_and_principal_bound() -> Result<()> {
        let server = crate::generate_key_pair()?;
        let client = crate::generate_key_pair()?;
        let config = ServerConfig::new(
            "vault",
            server.private,
            [(client.public, "codex".to_string())],
        )?;
        assert!(config.authorize(client.public, "codex"));
        assert!(!config.authorize(client.public, "root"));
        assert!(!config.authorize(server.public, "codex"));
        Ok(())
    }
}
