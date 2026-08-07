use crate::{PeerIdentity, PublicKey};
use r9p::error::{Error, Result, EPERM};
use std::{os::unix::net::UnixStream, sync::Arc};

pub const NOISE_SUBJECT_PREFIX: &str = "noise-static-key:";
/// A certificate binds the name to the key in signed material, so a relying
/// party can admit the name it learned at the handshake instead of keeping a
/// key list of its own. That list is the thing certificates exist to remove:
/// it makes every rotation a policy edit, and it lets two hosts disagree about
/// who a key is. Emitted alongside the key subject so policy can move one
/// entry at a time.
pub const CERT_SUBJECT_PREFIX: &str = "r9p-cert:";
pub const UNIX_PEER_SUBJECT_PREFIX: &str = "unix-peer:uid:";
pub const UNIX_SAME_USER_SUBJECT: &str = "unix-peer:same-user";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransportIdentity {
    Local,
    Authenticated {
        principal: Arc<str>,
        public_key: PublicKey,
        certified: bool,
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

    pub fn authenticated(peer: &PeerIdentity) -> Self {
        Self::Authenticated {
            principal: Arc::<str>::from(peer.principal()),
            public_key: peer.public_key(),
            certified: peer.certified(),
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
            Self::Authenticated {
                principal,
                certified: true,
                ..
            } => vec![
                self.subject_id(),
                CERT_SUBJECT_PREFIX.to_string() + principal,
            ],
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

    pub fn transport_authorizes_uname(&self, _uname: &str) -> bool {
        matches!(self, Self::Local)
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
    fn a_certificate_names_a_caller_without_admitting_it() -> Result<()> {
        let key = generate_key_pair()?;
        let peer = PeerIdentity::new("codex.interface", key.public)?;
        let identity = TransportIdentity::authenticated(&peer);

        assert_eq!(
            identity.subject_id(),
            format!("{NOISE_SUBJECT_PREFIX}{}", key.public)
        );
        assert_eq!(identity.authenticated_uname(), Some("codex.interface"));
        assert!(!identity.transport_authorizes_uname("codex.interface"));
        // An uncertified peer's name is only a claim, so it must not become a
        // subject a policy could admit by name.
        assert_eq!(
            identity.subject_ids(),
            vec![format!("{NOISE_SUBJECT_PREFIX}{}", key.public)]
        );

        let certified = TransportIdentity::authenticated(&peer.clone().into_certified());
        assert_eq!(
            certified.subject_ids(),
            vec![
                format!("{NOISE_SUBJECT_PREFIX}{}", key.public),
                format!("{CERT_SUBJECT_PREFIX}codex.interface"),
            ]
        );
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
