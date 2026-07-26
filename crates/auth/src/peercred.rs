use crate::{PeerIdentity, PublicKey};
use r9p::error::{Error, Result, EPERM};
use std::{os::unix::net::UnixStream, sync::Arc};

pub const NOISE_SUBJECT_PREFIX: &str = "noise-static-key:";
pub const UNIX_PEER_SUBJECT_PREFIX: &str = "unix-peer:uid:";
pub const UNIX_SAME_USER_SUBJECT: &str = "unix-peer:same-user";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransportIdentity {
    Local,
    Authenticated {
        principal: Arc<str>,
        public_key: PublicKey,
        preauthorized: bool,
    },
    UnixPeer {
        uid: u32,
        same_user: bool,
    },
}

impl TransportIdentity {
    pub const fn local() -> Self {
        Self::Local
    }

    pub fn authenticated(peer: &PeerIdentity, preauthorized: bool) -> Self {
        Self::Authenticated {
            principal: Arc::<str>::from(peer.principal()),
            public_key: peer.public_key(),
            preauthorized,
        }
    }

    pub fn unix_peer(stream: &UnixStream) -> Result<Self> {
        let credentials = rustix::net::sockopt::socket_peercred(stream)
            .map_err(|error| Error::from(format!("read unix peer credentials: {error}")))?;
        let uid = credentials.uid.as_raw();
        Ok(Self::UnixPeer {
            uid,
            same_user: uid == rustix::process::geteuid().as_raw(),
        })
    }

    pub fn same_user_local(stream: &UnixStream) -> Result<Self> {
        let identity = Self::unix_peer(stream)?;
        let Self::UnixPeer { same_user, .. } = identity else {
            unreachable!("unix peer identity has the wrong variant");
        };
        if same_user {
            Ok(Self::Local)
        } else {
            Err(Error::from(EPERM))
        }
    }

    pub fn subject_id(&self) -> String {
        match self {
            Self::Local => "local-trust".to_string(),
            Self::Authenticated { public_key, .. } => {
                NOISE_SUBJECT_PREFIX.to_string() + &public_key.to_hex()
            }
            Self::UnixPeer { uid, .. } => UNIX_PEER_SUBJECT_PREFIX.to_string() + &uid.to_string(),
        }
    }

    pub fn subject_ids(&self) -> Vec<String> {
        match self {
            Self::UnixPeer {
                same_user: true, ..
            } => vec![self.subject_id(), UNIX_SAME_USER_SUBJECT.to_string()],
            Self::Local | Self::Authenticated { .. } | Self::UnixPeer { .. } => {
                vec![self.subject_id()]
            }
        }
    }

    pub fn authenticated_uname(&self) -> Option<&str> {
        match self {
            Self::Authenticated { principal, .. } => Some(principal),
            Self::Local | Self::UnixPeer { .. } => None,
        }
    }

    pub fn transport_authorizes_uname(&self, uname: &str) -> bool {
        match self {
            Self::Local => true,
            Self::Authenticated {
                principal,
                preauthorized: true,
                ..
            } => principal.as_ref() == uname,
            Self::Authenticated {
                preauthorized: false,
                ..
            }
            | Self::UnixPeer { .. } => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate_key_pair;

    #[test]
    fn unix_identity_attests_the_kernel_peer_without_assigning_a_principal() -> Result<()> {
        let (client, server) =
            UnixStream::pair().map_err(|error| Error::from(error.to_string()))?;
        let uid = rustix::net::sockopt::socket_peercred(&server)
            .map_err(|error| Error::from(error.to_string()))?
            .uid
            .as_raw();
        let identity = TransportIdentity::unix_peer(&server)?;

        assert_eq!(
            identity.subject_id(),
            format!("{UNIX_PEER_SUBJECT_PREFIX}{uid}")
        );
        assert!(identity
            .subject_ids()
            .contains(&UNIX_SAME_USER_SUBJECT.to_string()));
        assert_eq!(identity.authenticated_uname(), None);
        assert!(!identity.transport_authorizes_uname("/srv/infra/agents"));
        drop(client);
        Ok(())
    }

    #[test]
    fn authenticated_identity_separates_attestation_from_preauthorization() -> Result<()> {
        let key = generate_key_pair()?;
        let peer = PeerIdentity::new("codex", key.public)?;
        let deferred = TransportIdentity::authenticated(&peer, false);
        let preauthorized = TransportIdentity::authenticated(&peer, true);

        assert_eq!(
            deferred.subject_id(),
            format!("{NOISE_SUBJECT_PREFIX}{}", key.public)
        );
        assert!(!deferred.transport_authorizes_uname("codex"));
        assert!(preauthorized.transport_authorizes_uname("codex"));
        assert!(!preauthorized.transport_authorizes_uname("root"));
        Ok(())
    }

    #[test]
    fn same_user_control_connection_is_explicit_local_trust() -> Result<()> {
        let (client, server) =
            UnixStream::pair().map_err(|error| Error::from(error.to_string()))?;
        assert_eq!(
            TransportIdentity::same_user_local(&server)?,
            TransportIdentity::Local
        );
        drop(client);
        Ok(())
    }
}
