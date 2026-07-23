use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use r9p::connection_descriptor::ConnectionDescriptor;

use crate::{Client, ConnectionConfig, Error, Result, ORDWR};

const CONNECTION_SERVER_PATH: &str = "/cs";
const MAX_CONNECTION_DESCRIPTOR_BYTES: u32 = 64 * 1024;

/// Local materialization of authority boundaries named by connection
/// descriptors.
///
/// Descriptors carry portable authority identities. Each host independently
/// maps an identity to its local session-auth configuration without leaking a
/// host path into the namespace contract.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AuthorityBindings {
    session_auth: BTreeMap<String, PathBuf>,
}

impl AuthorityBindings {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bind_session_auth(
        mut self,
        authority_boundary: impl Into<String>,
        config_path: impl Into<PathBuf>,
    ) -> Result<Self> {
        let authority_boundary = authority_boundary.into();
        let config_path = config_path.into();
        validate_authority_binding(&authority_boundary, &config_path)?;
        if self
            .session_auth
            .insert(authority_boundary.clone(), config_path)
            .is_some()
        {
            return Err(Error::new(
                libc::EEXIST,
                format!("duplicate authority binding {authority_boundary}"),
            ));
        }
        Ok(self)
    }

    fn session_auth_config(&self, authority_boundary: &str) -> Result<Option<PathBuf>> {
        if let Some(path) = self.session_auth.get(authority_boundary) {
            return Ok(Some(path.clone()));
        }
        if contained_or_network_authority(authority_boundary) {
            return Ok(None);
        }
        Err(Error::new(
            libc::EACCES,
            format!("authority boundary is not locally bound: {authority_boundary}"),
        ))
    }
}

#[derive(Clone, Debug)]
pub struct ResolvedNamespaceConfig {
    pub resolver: ConnectionConfig,
    pub service: String,
    pub mount_path: String,
    pub service_msize: u32,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub authorities: AuthorityBindings,
}

#[derive(Clone, Debug)]
pub struct ResolvedTarget {
    descriptor: ConnectionDescriptor,
    connection: ConnectionConfig,
    resolved_at: Instant,
}

impl ResolvedTarget {
    pub fn descriptor(&self) -> &ConnectionDescriptor {
        &self.descriptor
    }

    pub fn connection(&self) -> &ConnectionConfig {
        &self.connection
    }

    pub fn validity_remaining(&self) -> Duration {
        Duration::from_millis(self.descriptor.valid_for_ms)
            .saturating_sub(self.resolved_at.elapsed())
    }

    pub fn connect(&self, timeout: Duration) -> Result<Client> {
        let remaining = self.validity_remaining();
        if remaining.is_zero() {
            return Err(Error::new(
                libc::ESTALE,
                format!(
                    "resolved service generation expired before connect: {}",
                    self.descriptor.service
                ),
            ));
        }
        Client::connect_with_timeout(&self.connection, timeout.min(remaining))
    }
}

#[derive(Clone)]
pub struct NamespaceClient {
    root: Client,
    mounts: Vec<NamespaceMount>,
}

#[derive(Clone)]
struct NamespaceMount {
    mount_path: String,
    exported_root: String,
    client: Client,
}

#[derive(Clone)]
pub struct ResolvedNamespace {
    target: ResolvedTarget,
    namespace: NamespaceClient,
}

impl ResolvedNamespace {
    pub fn connect(config: &ResolvedNamespaceConfig) -> Result<Self> {
        validate_resolution_config(config)?;
        let resolver = Client::connect_with_timeout(&config.resolver, config.connect_timeout)?;
        let target = resolver.resolve_service_timeout(
            &config.service,
            config.service_msize,
            &config.authorities,
            config.request_timeout,
        )?;
        let service = target.connect(config.connect_timeout)?;
        let namespace = NamespaceClient::new(resolver).mount(
            &config.mount_path,
            &target.descriptor.exported_root,
            service,
        )?;
        Ok(Self { target, namespace })
    }

    pub fn target(&self) -> &ResolvedTarget {
        &self.target
    }

    pub fn namespace(&self) -> NamespaceClient {
        self.namespace.clone()
    }

    pub fn into_parts(self) -> (ResolvedTarget, NamespaceClient) {
        (self.target, self.namespace)
    }
}

impl NamespaceClient {
    pub fn new(root: Client) -> Self {
        Self {
            root,
            mounts: Vec::new(),
        }
    }

    pub fn mount(mut self, mount_path: &str, exported_root: &str, client: Client) -> Result<Self> {
        validate_mount_path(mount_path)?;
        validate_exported_root(exported_root)?;
        if self
            .mounts
            .iter()
            .any(|mount| mount.mount_path == mount_path)
        {
            return Err(Error::new(
                libc::EEXIST,
                format!("namespace mount path already exists: {mount_path}"),
            ));
        }
        self.mounts.push(NamespaceMount {
            mount_path: mount_path.to_string(),
            exported_root: exported_root.to_string(),
            client,
        });
        self.mounts.sort_by(|left, right| {
            right
                .mount_path
                .len()
                .cmp(&left.mount_path.len())
                .then_with(|| left.mount_path.cmp(&right.mount_path))
        });
        Ok(self)
    }

    pub fn open_path_timeout(
        &self,
        path: &str,
        mode: u8,
        timeout: Duration,
    ) -> Result<crate::OpenedFid> {
        let (client, routed_path) = self.route(path)?;
        client.open_path_timeout(&routed_path, mode, timeout)
    }

    pub fn shutdown(&self) -> Result<()> {
        let mut first_error = None;
        for mount in &self.mounts {
            if let Err(error) = mount.client.shutdown() {
                first_error.get_or_insert(error);
            }
        }
        if let Err(error) = self.root.shutdown() {
            first_error.get_or_insert(error);
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn route(&self, path: &str) -> Result<(&Client, String)> {
        validate_namespace_path(path)?;
        for mount in &self.mounts {
            if let Some(suffix) = mounted_suffix(path, &mount.mount_path) {
                return Ok((
                    &mount.client,
                    join_namespace_path(&mount.exported_root, suffix),
                ));
            }
        }
        Ok((&self.root, path.to_string()))
    }
}

impl Client {
    pub fn resolve_service_timeout(
        &self,
        service: &str,
        service_msize: u32,
        authorities: &AuthorityBindings,
        timeout: Duration,
    ) -> Result<ResolvedTarget> {
        validate_service_name(service)?;
        if service_msize < 1024 {
            return Err(Error::new(
                libc::EINVAL,
                "resolved service msize must be at least 1024",
            ));
        }
        if timeout.is_zero() {
            return Err(Error::new(
                libc::EINVAL,
                "service resolution timeout must be nonzero",
            ));
        }

        let mut fid = self.open_path_timeout(CONNECTION_SERVER_PATH, ORDWR, timeout)?;
        let mut request = Vec::with_capacity(service.len() + 1);
        request.extend_from_slice(service.as_bytes());
        request.push(b'\n');
        let written = fid.write_timeout(0, &request, timeout)?;
        if usize::try_from(written).ok() != Some(request.len()) {
            return Err(Error::new(
                libc::EIO,
                format!(
                    "connection server accepted {written} of {} request bytes",
                    request.len()
                ),
            ));
        }
        let bytes = fid.read_full_timeout(0, MAX_CONNECTION_DESCRIPTOR_BYTES, timeout)?;
        fid.close()?;
        let rendered = std::str::from_utf8(&bytes).map_err(|_| {
            Error::new(
                libc::EPROTO,
                "connection server returned a non-UTF-8 descriptor",
            )
        })?;
        let descriptor = ConnectionDescriptor::parse(rendered)
            .map_err(|error| Error::new(libc::EPROTO, error.display_lossy().to_string()))?;
        if descriptor.service != service {
            return Err(Error::new(
                libc::EPROTO,
                format!(
                    "connection server resolved service {} for request {service}",
                    descriptor.service
                ),
            ));
        }
        let auth_config =
            authorities.session_auth_config(descriptor.authority_boundary.as_str())?;
        let connection = ConnectionConfig {
            address: descriptor.endpoint_bind.clone(),
            uname: descriptor.uname.clone(),
            aname: descriptor.aname.clone(),
            msize: service_msize,
            auth_config,
        };
        Ok(ResolvedTarget {
            descriptor,
            connection,
            resolved_at: Instant::now(),
        })
    }
}

fn validate_resolution_config(config: &ResolvedNamespaceConfig) -> Result<()> {
    validate_service_name(&config.service)?;
    validate_mount_path(&config.mount_path)?;
    if config.connect_timeout.is_zero() {
        return Err(Error::new(
            libc::EINVAL,
            "resolved namespace connect timeout must be nonzero",
        ));
    }
    if config.request_timeout.is_zero() {
        return Err(Error::new(
            libc::EINVAL,
            "resolved namespace request timeout must be nonzero",
        ));
    }
    if config.service_msize < 1024 {
        return Err(Error::new(
            libc::EINVAL,
            "resolved service msize must be at least 1024",
        ));
    }
    Ok(())
}

fn validate_service_name(service: &str) -> Result<()> {
    if service.is_empty()
        || service.starts_with('/')
        || service.ends_with('/')
        || service.split('/').any(|segment| segment.is_empty())
        || service
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
    {
        return Err(Error::new(
            libc::EINVAL,
            format!("invalid connection-server service name {service:?}"),
        ));
    }
    Ok(())
}

fn validate_authority_binding(authority_boundary: &str, config_path: &Path) -> Result<()> {
    if authority_boundary.is_empty()
        || authority_boundary
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
    {
        return Err(Error::new(
            libc::EINVAL,
            "authority binding name is invalid",
        ));
    }
    if !config_path.is_absolute() {
        return Err(Error::new(
            libc::EINVAL,
            format!(
                "authority binding config path must be absolute: {}",
                config_path.display()
            ),
        ));
    }
    Ok(())
}

fn contained_or_network_authority(authority_boundary: &str) -> bool {
    matches!(authority_boundary, "loopback" | "unix_socket")
        || authority_boundary.starts_with("network_class:")
}

fn validate_mount_path(path: &str) -> Result<()> {
    validate_namespace_path(path)?;
    if path == "/" {
        return Err(Error::new(
            libc::EINVAL,
            "namespace mount path cannot replace the root",
        ));
    }
    Ok(())
}

fn validate_exported_root(path: &str) -> Result<()> {
    validate_namespace_path(path)
}

fn validate_namespace_path(path: &str) -> Result<()> {
    if path == "/" {
        return Ok(());
    }
    if !path.starts_with('/')
        || (path.len() > 1 && path.ends_with('/'))
        || path
            .split('/')
            .skip(1)
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
        || path
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
    {
        return Err(Error::new(
            libc::EINVAL,
            format!("invalid absolute namespace path {path:?}"),
        ));
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

    #[test]
    fn authority_bindings_require_exact_portable_identity() {
        let bindings = AuthorityBindings::new()
            .bind_session_auth(
                "p9any:noise-ik@agents",
                "/etc/r9p-session-auth/agents-client.conf",
            )
            .expect("binding should be valid");

        assert_eq!(
            bindings
                .session_auth_config("p9any:noise-ik@agents")
                .expect("binding should resolve"),
            Some(PathBuf::from("/etc/r9p-session-auth/agents-client.conf"))
        );
        assert!(bindings
            .session_auth_config("p9any:noise-ik@another-service")
            .is_err());
    }

    #[test]
    fn contained_and_explicit_network_boundaries_need_no_session_config() {
        let bindings = AuthorityBindings::new();
        for boundary in ["loopback", "unix_socket", "network_class:tailnet"] {
            assert_eq!(
                bindings
                    .session_auth_config(boundary)
                    .expect("boundary should resolve"),
                None
            );
        }
    }

    #[test]
    fn resolution_rejects_path_shaped_service_names() {
        for service in ["", "/infra/agents", "infra/agents/", "infra//agents"] {
            assert!(
                validate_service_name(service).is_err(),
                "accepted {service:?}"
            );
        }
        assert!(validate_service_name("infra/agents").is_ok());
    }

    #[test]
    fn mounted_paths_rebase_onto_exported_root() {
        assert_eq!(mounted_suffix("/agents", "/agents"), Some(""));
        assert_eq!(
            mounted_suffix("/agents/terminals/a", "/agents"),
            Some("/terminals/a")
        );
        assert_eq!(mounted_suffix("/agents-extra", "/agents"), None);
        assert_eq!(join_namespace_path("/", "/terminals/a"), "/terminals/a");
        assert_eq!(
            join_namespace_path("/exported", "/terminals/a"),
            "/exported/terminals/a"
        );
    }

    #[test]
    fn namespace_paths_are_canonical_and_absolute() {
        for path in [
            "",
            "agents",
            "/agents/",
            "/agents//status",
            "/agents/../status",
        ] {
            assert!(
                validate_namespace_path(path).is_err(),
                "accepted invalid namespace path {path:?}"
            );
        }
        assert!(validate_namespace_path("/").is_ok());
        assert!(validate_namespace_path("/agents/terminals/a").is_ok());
        assert!(validate_mount_path("/").is_err());
    }
}
