use crate::{
    cert::{Certificate, RootPublicKey},
    key::derive_public_key,
    PrivateKey, PublicKey, CONFIG_FORMAT,
};
use r9p::error::{Error, Result};
use r9p::export_descriptor::validate_p9any_domain;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

const MAX_PRINCIPAL_BYTES: usize = 255;

#[derive(Clone, Debug)]
pub struct ClientConfig {
    /// Absent when one config serves many services: the domain then comes from
    /// the connection, which is what removes per-service client configuration.
    domain: Option<String>,
    private_key: PrivateKey,
    /// Only IK needs this. Under XX the responder's key arrives in the
    /// handshake and is authenticated by its certificate instead of pinned.
    server_key: Option<PublicKey>,
    certificate: Option<Certificate>,
    roots: Vec<RootPublicKey>,
}

#[derive(Clone, Debug)]
pub struct ServerConfig {
    domain: String,
    private_key: PrivateKey,
    peers: BTreeMap<PublicKey, BTreeSet<String>>,
    /// This server's own identity, presented to clients under XX.
    certificate: Option<Certificate>,
    /// Roots whose certificates this server accepts. A server with roots and no
    /// peers authorizes purely on signed names; one with both is mid-migration.
    roots: Vec<RootPublicKey>,
}

impl ClientConfig {
    pub fn new(
        domain: impl Into<String>,
        private_key: PrivateKey,
        server_key: PublicKey,
    ) -> Result<Self> {
        let domain = domain.into();
        validate_p9any_domain(&domain)?;
        Ok(Self {
            domain: Some(domain),
            private_key,
            server_key: Some(server_key),
            certificate: None,
            roots: Vec::new(),
        })
    }

    /// The config that ends pairwise client setup: an identity and the roots it
    /// trusts, with no service named and no responder key pinned. The domain is
    /// supplied per connection and checked against the responder's certificate.
    pub fn certified(
        private_key: PrivateKey,
        certificate: Certificate,
        roots: impl IntoIterator<Item = RootPublicKey>,
    ) -> Result<Self> {
        let roots = dedupe_roots(roots);
        if roots.is_empty() {
            return Err(Error::from(
                "a certified client config requires at least one root",
            ));
        }
        Self {
            domain: None,
            private_key,
            server_key: None,
            certificate: None,
            roots,
        }
        .with_certificate(certificate)
    }

    pub fn with_roots(mut self, roots: impl IntoIterator<Item = RootPublicKey>) -> Self {
        self.roots = dedupe_roots(roots);
        self
    }

    pub fn roots(&self) -> &[RootPublicKey] {
        &self.roots
    }

    /// Whether this client can authenticate a responder from its certificate,
    /// which is what lets it select XX and stop pinning keys.
    pub fn can_verify_responder(&self) -> bool {
        self.certificate.is_some() && !self.roots.is_empty()
    }

    /// Attaches the certificate this client presents instead of a bare name.
    /// Rejects one issued for a different key, so a misconfigured pairing fails
    /// here rather than as an opaque handshake rejection on the far side.
    pub fn with_certificate(mut self, certificate: Certificate) -> Result<Self> {
        let own = derive_public_key(&self.private_key)?;
        if certificate.body().key() != own {
            return Err(Error::from(format!(
                "certificate for {} was issued for another session key",
                certificate.body().name()
            )));
        }
        self.certificate = Some(certificate);
        Ok(self)
    }

    pub const fn certificate(&self) -> Option<&Certificate> {
        self.certificate.as_ref()
    }

    /// Attaches a certificate without the key-binding check, so tests can prove
    /// the *server* refuses a replayed certificate rather than only proving
    /// `with_certificate` refuses to build one.
    #[cfg(test)]
    pub(crate) fn force_certificate_for_test(&mut self, certificate: Certificate) {
        self.certificate = Some(certificate);
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
        let domain = match parsed.domain {
            Some(domain) => {
                validate_p9any_domain(&domain)?;
                Some(domain)
            }
            None => None,
        };
        let server_key = match parsed.server_key {
            Some(value) => Some(PublicKey::from_hex(&value)?),
            None => None,
        };
        let mut roots = Vec::with_capacity(parsed.roots.len());
        for root in parsed.roots {
            roots.push(RootPublicKey::from_hex(&root)?);
        }
        let config = Self {
            domain,
            private_key: PrivateKey::read(&private_path)?,
            server_key,
            certificate: None,
            roots: dedupe_roots(roots),
        };
        let config = match parsed.certificate {
            None => config,
            Some(value) => {
                let certificate_path = resolve_path(path, value);
                config.with_certificate(Certificate::read(&certificate_path)?)?
            }
        };
        // Either it pins a responder key (IK) or it can authenticate one from a
        // certificate (XX). Neither leaves nothing to trust.
        if config.server_key.is_none() && !config.can_verify_responder() {
            return Err(Error::from(
                "client session auth config needs either server-key or a certificate with a root",
            ));
        }
        Ok(config)
    }

    pub fn domain(&self) -> Option<&str> {
        self.domain.as_deref()
    }

    pub fn private_key(&self) -> &PrivateKey {
        &self.private_key
    }

    pub const fn server_key(&self) -> Option<PublicKey> {
        self.server_key
    }
}

fn dedupe_roots(roots: impl IntoIterator<Item = RootPublicKey>) -> Vec<RootPublicKey> {
    let mut accepted = Vec::new();
    for root in roots {
        if !accepted.contains(&root) {
            accepted.push(root);
        }
    }
    accepted
}

impl ServerConfig {
    pub fn new(
        domain: impl Into<String>,
        private_key: PrivateKey,
        peers: impl IntoIterator<Item = (PublicKey, String)>,
    ) -> Result<Self> {
        Self::new_with_roots(domain, private_key, peers, Vec::new())
    }

    /// A server may authorize by listed peer, by certificate root, or both.
    /// Both is the migration state: existing peers keep working while
    /// certificate-bearing clients are cut over.
    pub fn new_with_roots(
        domain: impl Into<String>,
        private_key: PrivateKey,
        peers: impl IntoIterator<Item = (PublicKey, String)>,
        roots: impl IntoIterator<Item = RootPublicKey>,
    ) -> Result<Self> {
        let domain = domain.into();
        validate_p9any_domain(&domain)?;
        let mut allowed = BTreeMap::<PublicKey, BTreeSet<String>>::new();
        for (key, principal) in peers {
            validate_principal(&principal)?;
            if !allowed.entry(key).or_default().insert(principal.clone()) {
                return Err(Error::from(format!(
                    "duplicate authorized peer principal {principal}"
                )));
            }
        }
        let mut accepted = Vec::new();
        for root in roots {
            if !accepted.contains(&root) {
                accepted.push(root);
            }
        }
        if allowed.is_empty() && accepted.is_empty() {
            return Err(Error::from(
                "server session auth config requires at least one peer or root",
            ));
        }
        Ok(Self {
            domain,
            private_key,
            peers: allowed,
            certificate: None,
            roots: accepted,
        })
    }

    pub fn roots(&self) -> &[RootPublicKey] {
        &self.roots
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
        let mut roots = Vec::with_capacity(parsed.roots.len());
        for root in parsed.roots {
            roots.push(RootPublicKey::from_hex(&root)?);
        }
        let config = Self::new_with_roots(
            required(parsed.domain, "domain")?,
            PrivateKey::read(&private_path)?,
            peers,
            roots,
        )?;
        match parsed.certificate {
            None => Ok(config),
            Some(value) => {
                let certificate_path = resolve_path(path, value);
                config.with_certificate(Certificate::read(&certificate_path)?)
            }
        }
    }

    /// The certificate this server presents under XX. Its name must equal the
    /// domain clients ask for, so a client that asked for `terminal-m7` cannot
    /// be answered by a differently-named service holding a valid certificate.
    pub fn with_certificate(mut self, certificate: Certificate) -> Result<Self> {
        let own = derive_public_key(&self.private_key)?;
        if certificate.body().key() != own {
            return Err(Error::from(format!(
                "certificate for {} was issued for another session key",
                certificate.body().name()
            )));
        }
        if certificate.body().name() != self.domain {
            return Err(Error::from(format!(
                "certificate names {} but this server serves domain {}",
                certificate.body().name(),
                self.domain
            )));
        }
        self.certificate = Some(certificate);
        Ok(self)
    }

    pub const fn certificate(&self) -> Option<&Certificate> {
        self.certificate.as_ref()
    }

    /// Attaches a certificate without the domain check, so tests can build a
    /// server that lies about which service it is and prove clients catch it.
    #[cfg(test)]
    pub(crate) fn force_certificate_for_test(&mut self, certificate: Certificate) {
        self.certificate = Some(certificate);
    }

    /// Whether this server can prove its own identity, and so offer XX.
    pub fn can_prove_identity(&self) -> bool {
        self.certificate.is_some() && !self.roots.is_empty()
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

#[derive(Default)]
struct ParsedConfig {
    role: Option<String>,
    domain: Option<String>,
    private_key: Option<String>,
    server_key: Option<String>,
    certificate: Option<String>,
    peers: Vec<(String, String)>,
    roots: Vec<String>,
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
                ("certificate", [value]) => {
                    set_once(&mut parsed.certificate, value, "certificate")?
                }
                ("peer", [key, principal]) => {
                    parsed
                        .peers
                        .push(((*key).to_string(), (*principal).to_string()));
                }
                ("root", [value]) => parsed.roots.push((*value).to_string()),
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

    use crate::cert::{generate_root_key_pair, CertificateBody};
    use std::sync::atomic::{AtomicU64, Ordering};

    static CONFIG_TEST_SERIAL: AtomicU64 = AtomicU64::new(0);

    fn test_root(label: &str) -> Result<PathBuf> {
        let serial = CONFIG_TEST_SERIAL.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "r9p-config-{label}-test-{}-{serial}",
            std::process::id()
        ));
        fs::create_dir(&root).map_err(|error| Error::from(error.to_string()))?;
        Ok(root)
    }

    fn write(path: &Path, body: &str) -> Result<()> {
        fs::write(path, body).map_err(|error| Error::from(error.to_string()))
    }

    #[test]
    fn a_server_config_may_carry_only_a_root() -> Result<()> {
        let dir = test_root("server-root")?;
        let root = generate_root_key_pair()?;
        let server = crate::generate_key_pair()?;
        let key_path = dir.join("private");
        crate::write_key_pair(&key_path, &dir.join("public"), &server)?;
        let config_path = dir.join("server.conf");
        write(
            &config_path,
            &format!(
                "format {CONFIG_FORMAT}\nrole server\ndomain vault\nprivate-key {}\nroot {}\n",
                key_path.display(),
                root.public
            ),
        )?;
        let parsed = ServerConfig::read(&config_path)?;
        assert_eq!(parsed.roots(), [root.public]);
        Ok(())
    }

    #[test]
    fn a_client_config_loads_and_binds_its_certificate() -> Result<()> {
        let dir = test_root("client-cert")?;
        let root = generate_root_key_pair()?;
        let server = crate::generate_key_pair()?;
        let client = crate::generate_key_pair()?;
        let key_path = dir.join("private");
        crate::write_key_pair(&key_path, &dir.join("public"), &client)?;

        let body = CertificateBody::new(
            "tuxedo",
            client.public,
            ["operator".to_string()],
            1_000,
            4_000_000_000,
            root.public,
        )?;
        let certificate = Certificate::sign(&root.private, body)?;
        let cert_path = dir.join("tuxedo.crt");
        certificate.write(&cert_path)?;

        let config_path = dir.join("client.conf");
        write(
            &config_path,
            &format!(
                "format {CONFIG_FORMAT}\nrole client\ndomain vault\nprivate-key {}\nserver-key {}\ncertificate {}\n",
                key_path.display(),
                server.public.to_hex(),
                cert_path.display()
            ),
        )?;
        let parsed = ClientConfig::read(&config_path)?;
        let loaded = parsed
            .certificate()
            .ok_or_else(|| Error::from("certificate was not loaded"))?;
        assert_eq!(loaded.body().name(), "tuxedo");
        assert_eq!(loaded.body().key(), client.public);
        Ok(())
    }

    #[test]
    fn a_certificate_for_a_different_key_is_refused_at_load() -> Result<()> {
        let root = generate_root_key_pair()?;
        let server = crate::generate_key_pair()?;
        let client = crate::generate_key_pair()?;
        let other = crate::generate_key_pair()?;
        let body = CertificateBody::new(
            "tuxedo",
            other.public,
            Vec::<String>::new(),
            1_000,
            4_000_000_000,
            root.public,
        )?;
        let certificate = Certificate::sign(&root.private, body)?;
        let config = ClientConfig::new("vault", client.private, server.public)?;
        assert!(config.with_certificate(certificate).is_err());
        Ok(())
    }

    #[test]
    fn a_client_may_trust_roots_and_pin_nothing() -> Result<()> {
        // Under XX the client authenticates the responder from its certificate,
        // so roots on a client are required rather than forbidden, and no
        // server-key is pinned.
        let dir = test_root("client-roots")?;
        let root = generate_root_key_pair()?;
        let client = crate::generate_key_pair()?;
        let key_path = dir.join("private");
        crate::write_key_pair(&key_path, &dir.join("public"), &client)?;
        let body = CertificateBody::new(
            "tuxedo",
            client.public,
            ["operator".to_string()],
            1_000,
            4_000_000_000,
            root.public,
        )?;
        let cert_path = dir.join("tuxedo.crt");
        Certificate::sign(&root.private, body)?.write(&cert_path)?;

        let config_path = dir.join("client.conf");
        write(
            &config_path,
            &format!(
                "format {CONFIG_FORMAT}\nrole client\nprivate-key {}\ncertificate {}\nroot {}\n",
                key_path.display(),
                cert_path.display(),
                root.public
            ),
        )?;
        let parsed = ClientConfig::read(&config_path)?;
        assert!(
            parsed.domain().is_none(),
            "a certified client pins no service"
        );
        assert!(
            parsed.server_key().is_none(),
            "a certified client pins no key"
        );
        assert!(parsed.can_verify_responder());
        Ok(())
    }

    #[test]
    fn a_client_that_can_neither_pin_nor_verify_is_refused() -> Result<()> {
        let dir = test_root("client-empty")?;
        let pair = crate::generate_key_pair()?;
        let key_path = dir.join("private");
        crate::write_key_pair(&key_path, &dir.join("public"), &pair)?;
        let config_path = dir.join("client.conf");
        write(
            &config_path,
            &format!(
                "format {CONFIG_FORMAT}\nrole client\ndomain vault\nprivate-key {}\n",
                key_path.display()
            ),
        )?;
        assert!(ClientConfig::read(&config_path).is_err());
        Ok(())
    }

    #[test]
    fn a_server_certificate_must_match_the_domain_it_serves() -> Result<()> {
        // Otherwise a validly-certified service could answer in another's place.
        let root = generate_root_key_pair()?;
        let server = crate::generate_key_pair()?;
        let body = CertificateBody::new(
            "terminal-nucbox",
            server.public,
            Vec::<String>::new(),
            1_000,
            4_000_000_000,
            root.public,
        )?;
        let certificate = Certificate::sign(&root.private, body)?;
        let config =
            ServerConfig::new_with_roots("terminal-m7", server.private, Vec::new(), [root.public])?;
        assert!(config.with_certificate(certificate).is_err());
        Ok(())
    }

    #[test]
    fn a_server_config_with_neither_peer_nor_root_is_refused() -> Result<()> {
        let pair = crate::generate_key_pair()?;
        assert!(
            ServerConfig::new_with_roots("vault", pair.private, Vec::new(), Vec::new()).is_err()
        );
        Ok(())
    }
}
