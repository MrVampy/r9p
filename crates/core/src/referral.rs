use crate::{Error, Result};

/// An admitted direct target for one mounted prefix in a composed namespace.
///
/// Referrals are protocol mechanism. They are exchanged through the r9p
/// dialect and are not files in the namespace being composed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamespaceReferral {
    pub mount_path: Vec<u8>,
    pub endpoint: Vec<u8>,
    pub uname: Vec<u8>,
    pub aname: Vec<u8>,
    pub exported_root: Vec<u8>,
    pub authority_boundary: Vec<u8>,
    pub generation: u64,
    pub valid_for_ms: u64,
}

impl NamespaceReferral {
    pub fn validate(&self) -> Result<()> {
        validate_absolute_path("mount_path", &self.mount_path, false)?;
        validate_absolute_path("exported_root", &self.exported_root, true)?;
        for (field, value) in [
            ("endpoint", self.endpoint.as_slice()),
            ("uname", self.uname.as_slice()),
            ("aname", self.aname.as_slice()),
            ("authority_boundary", self.authority_boundary.as_slice()),
        ] {
            validate_nonempty(field, value)?;
            if value
                .iter()
                .any(|byte| *byte == 0 || byte.is_ascii_control())
            {
                return Err(Error::from(format!(
                    "namespace referral {field} contains a control byte"
                )));
            }
        }
        if self.generation == 0 {
            return Err(Error::from(
                "namespace referral generation must be positive",
            ));
        }
        if self.valid_for_ms == 0 {
            return Err(Error::from(
                "namespace referral valid_for_ms must be positive",
            ));
        }
        Ok(())
    }

    pub fn routed_path(&self, namespace_path: &[u8]) -> Result<Vec<u8>> {
        validate_absolute_path("namespace_path", namespace_path, true)?;
        let suffix = mounted_suffix(namespace_path, &self.mount_path).ok_or_else(|| {
            Error::from(format!(
                "namespace path {} is outside referral mount {}",
                String::from_utf8_lossy(namespace_path),
                String::from_utf8_lossy(&self.mount_path)
            ))
        })?;
        Ok(join_namespace_path(&self.exported_root, suffix))
    }
}

fn validate_nonempty(field: &str, value: &[u8]) -> Result<()> {
    if value.is_empty() {
        return Err(Error::from(format!(
            "namespace referral {field} must not be empty"
        )));
    }
    Ok(())
}

fn validate_absolute_path(field: &str, path: &[u8], allow_root: bool) -> Result<()> {
    let valid = if path == b"/" {
        allow_root
    } else {
        path.starts_with(b"/")
            && !path.ends_with(b"/")
            && path
                .split(|byte| *byte == b'/')
                .skip(1)
                .all(|segment| !segment.is_empty() && segment != b"." && segment != b"..")
            && !path
                .iter()
                .any(|byte| *byte == 0 || byte.is_ascii_control())
    };
    if valid {
        Ok(())
    } else {
        Err(Error::from(format!(
            "namespace referral {field} must be a canonical absolute path"
        )))
    }
}

fn mounted_suffix<'a>(path: &'a [u8], mount_path: &[u8]) -> Option<&'a [u8]> {
    if path == mount_path {
        return Some(&[]);
    }
    path.strip_prefix(mount_path)
        .filter(|suffix| suffix.starts_with(b"/"))
}

fn join_namespace_path(root: &[u8], suffix: &[u8]) -> Vec<u8> {
    match (root, suffix) {
        (b"/", b"") => b"/".to_vec(),
        (b"/", suffix) => suffix.to_vec(),
        (root, b"") => root.to_vec(),
        (root, suffix) => [root, suffix].concat(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn referral() -> NamespaceReferral {
        NamespaceReferral {
            mount_path: b"/agents".to_vec(),
            endpoint: b"192.168.0.30:9660".to_vec(),
            uname: b"codex".to_vec(),
            aname: b"/".to_vec(),
            exported_root: b"/".to_vec(),
            authority_boundary: b"p9any:noise-xx@agents".to_vec(),
            generation: 3,
            valid_for_ms: 10_000,
        }
    }

    #[test]
    fn validates_and_rebases_namespace_paths() {
        let referral = referral();
        referral.validate().expect("valid referral");
        assert_eq!(
            referral
                .routed_path(b"/agents/terminals/t-1")
                .expect("routed path"),
            b"/terminals/t-1"
        );
        assert!(referral.routed_path(b"/agents-extra").is_err());
    }

    #[test]
    fn rejects_root_mount_and_expired_referral() {
        let mut referral = referral();
        referral.mount_path = b"/".to_vec();
        assert!(referral.validate().is_err());
        referral.mount_path = b"/agents".to_vec();
        referral.valid_for_ms = 0;
        assert!(referral.validate().is_err());
    }
}
