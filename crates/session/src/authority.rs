use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use crate::{Error, Result};

/// Local credential material bound to portable session authority names.
///
/// Referrals name an authority boundary, never a host-local credential path.
/// The client host supplies this mapping when it mounts the namespace.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct AuthorityBindings {
    session_auth: BTreeMap<String, PathBuf>,
}

impl AuthorityBindings {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_session_auth(
        &mut self,
        authority_boundary: impl Into<String>,
        config_path: impl Into<PathBuf>,
    ) -> Result<()> {
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
        Ok(())
    }

    pub fn bind_session_auth(
        mut self,
        authority_boundary: impl Into<String>,
        config_path: impl Into<PathBuf>,
    ) -> Result<Self> {
        self.insert_session_auth(authority_boundary, config_path)?;
        Ok(self)
    }

    pub(crate) fn session_auth_config(
        &self,
        authority_boundary: &str,
    ) -> Result<Option<PathBuf>> {
        if let Some(path) = self.session_auth.get(authority_boundary) {
            return Ok(Some(path.clone()));
        }
        if contained_authority(authority_boundary) {
            return Ok(None);
        }
        Err(Error::new(
            libc::EACCES,
            format!("authority boundary is not locally bound: {authority_boundary}"),
        ))
    }
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

fn contained_authority(authority_boundary: &str) -> bool {
    matches!(authority_boundary, "loopback" | "unix_socket")
        || authority_boundary.starts_with("network_class:")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bindings_require_an_exact_portable_identity() {
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
    fn contained_boundaries_need_no_session_config() {
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
}
