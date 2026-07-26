use crate::{config::validate_principal, PeerIdentity, PEER_CREDENTIAL_CONFIG_FORMAT};
use r9p::error::{Error, Result, EPERM};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::net::UnixStream,
    path::Path,
    sync::Arc,
};

#[derive(Clone, Debug)]
pub struct PeerCredentialConfig {
    principals: Arc<BTreeMap<u32, BTreeSet<String>>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransportIdentity {
    Local,
    Authenticated(Arc<str>),
    UnixPeer {
        uid: u32,
        principals: Arc<BTreeSet<String>>,
    },
}

impl PeerCredentialConfig {
    pub fn new(entries: impl IntoIterator<Item = (u32, String)>) -> Result<Self> {
        let mut principals = BTreeMap::<u32, BTreeSet<String>>::new();
        for (uid, principal) in entries {
            validate_uid(uid)?;
            validate_principal(&principal)?;
            if !principals.entry(uid).or_default().insert(principal.clone()) {
                return Err(Error::from(format!(
                    "duplicate peer credential principal {uid} {principal}"
                )));
            }
        }
        if principals.is_empty() {
            return Err(Error::from(
                "peer credential config requires at least one peer",
            ));
        }
        Ok(Self {
            principals: Arc::new(principals),
        })
    }

    pub fn read(path: &Path) -> Result<Self> {
        let input = fs::read_to_string(path).map_err(|error| {
            Error::from(format!(
                "read peer credential config {}: {error}",
                path.display()
            ))
        })?;
        let mut format = None;
        let mut entries = Vec::new();
        for (index, raw_line) in input.lines().enumerate() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
            match fields.as_slice() {
                ["format", value] if format.is_none() => {
                    format = Some((*value).to_string());
                }
                ["format", _] => {
                    return Err(Error::from(
                        "duplicate peer credential config field format",
                    ));
                }
                ["peer", uid, principal] => {
                    let uid = uid.parse::<u32>().map_err(|_| {
                        Error::from(format!(
                            "invalid peer credential uid on line {} in {}",
                            index + 1,
                            path.display()
                        ))
                    })?;
                    entries.push((uid, (*principal).to_string()));
                }
                _ => {
                    return Err(Error::from(format!(
                        "invalid peer credential config line {} in {}",
                        index + 1,
                        path.display()
                    )));
                }
            }
        }
        if format.as_deref() != Some(PEER_CREDENTIAL_CONFIG_FORMAT) {
            return Err(Error::from(format!(
                "peer credential config format must be {PEER_CREDENTIAL_CONFIG_FORMAT}"
            )));
        }
        Self::new(entries)
    }

    pub fn identity(&self, stream: &UnixStream) -> Result<TransportIdentity> {
        let credentials = rustix::net::sockopt::socket_peercred(stream)
            .map_err(|error| Error::from(format!("read unix peer credentials: {error}")))?;
        let uid = credentials.uid.as_raw();
        let principals = self
            .principals
            .get(&uid)
            .cloned()
            .ok_or_else(|| Error::from(EPERM))?;
        Ok(TransportIdentity::UnixPeer {
            uid,
            principals: Arc::new(principals),
        })
    }

    pub fn authorize(&self, uid: u32, principal: &str) -> bool {
        self.principals
            .get(&uid)
            .is_some_and(|principals| principals.contains(principal))
    }
}

impl TransportIdentity {
    pub const fn local() -> Self {
        Self::Local
    }

    pub fn authenticated(peer: &PeerIdentity) -> Self {
        Self::authenticated_principal(peer.principal())
    }

    pub fn authenticated_principal(principal: &str) -> Self {
        Self::Authenticated(Arc::<str>::from(principal))
    }

    pub fn authenticated_uname(&self) -> Option<&str> {
        match self {
            Self::Authenticated(principal) => Some(principal),
            Self::Local | Self::UnixPeer { .. } => None,
        }
    }

    pub fn authorize_uname(&self, uname: &str) -> Result<()> {
        match self {
            Self::Local => Ok(()),
            Self::Authenticated(principal) if principal.as_ref() == uname => Ok(()),
            Self::UnixPeer { principals, .. } if principals.contains(uname) => Ok(()),
            Self::Authenticated(_) | Self::UnixPeer { .. } => Err(Error::from(EPERM)),
        }
    }
}

fn validate_uid(uid: u32) -> Result<()> {
    if uid == u32::MAX {
        Err(Error::from("invalid peer credential uid"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_credential_authority_is_uid_and_principal_bound() -> Result<()> {
        let config = PeerCredentialConfig::new([
            (992, "/srv/infra/agents".to_string()),
            (993, "coordinator.runtime".to_string()),
            (993, "coordinator.runtime-maintainer".to_string()),
        ])?;

        assert!(config.authorize(992, "/srv/infra/agents"));
        assert!(!config.authorize(992, "coordinator.runtime"));
        assert!(config.authorize(993, "coordinator.runtime"));
        assert!(config.authorize(993, "coordinator.runtime-maintainer"));
        Ok(())
    }

    #[test]
    fn unix_identity_rejects_an_unconfigured_peer() -> Result<()> {
        let (client, server) = UnixStream::pair().map_err(|error| Error::from(error.to_string()))?;
        let uid = rustix::net::sockopt::socket_peercred(&server)
            .map_err(|error| Error::from(error.to_string()))?
            .uid
            .as_raw();
        let configured_uid = uid.wrapping_add(1);
        let config =
            PeerCredentialConfig::new([(configured_uid, "example".to_string())])?;

        assert!(config.identity(&server).is_err());
        drop(client);
        Ok(())
    }

    #[test]
    fn unix_identity_admits_only_configured_principals() -> Result<()> {
        let (client, server) = UnixStream::pair().map_err(|error| Error::from(error.to_string()))?;
        let uid = rustix::net::sockopt::socket_peercred(&server)
            .map_err(|error| Error::from(error.to_string()))?
            .uid
            .as_raw();
        let config = PeerCredentialConfig::new([
            (uid, "coordinator.runtime".to_string()),
            (uid, "coordinator.runtime-maintainer".to_string()),
        ])?;
        let identity = config.identity(&server)?;

        identity.authorize_uname("coordinator.runtime")?;
        identity.authorize_uname("coordinator.runtime-maintainer")?;
        assert!(identity.authorize_uname("root").is_err());
        drop(client);
        Ok(())
    }
}
