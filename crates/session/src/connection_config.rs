use std::path::{Path, PathBuf};

use crate::{Error, Result};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ClientCredential {
    config: PathBuf,
}

impl ClientCredential {
    pub fn new(config: impl Into<PathBuf>) -> Result<Self> {
        let config = config.into();
        if config.as_os_str().is_empty() {
            return Err(Error::new(
                libc::EINVAL,
                "a session credential needs a config path",
            ));
        }
        Ok(Self { config })
    }

    pub fn config(&self) -> &Path {
        &self.config
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ResponderName(String);

impl ResponderName {
    pub fn new(name: impl Into<String>) -> Result<Self> {
        let name = name.into();
        r9p::export_descriptor::validate_p9any_domain(&name)
            .map_err(|error| Error::new(libc::EINVAL, error.to_string()))?;
        Ok(Self(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SessionAuthentication {
    credential: ClientCredential,
    root_responder: Option<ResponderName>,
}

impl SessionAuthentication {
    pub fn contained_root(credential: ClientCredential) -> Self {
        Self {
            credential,
            root_responder: None,
        }
    }

    pub fn authenticated_root(credential: ClientCredential, root_responder: ResponderName) -> Self {
        Self {
            credential,
            root_responder: Some(root_responder),
        }
    }

    pub fn for_responder(&self, responder: ResponderName) -> Self {
        Self {
            credential: self.credential.clone(),
            root_responder: Some(responder),
        }
    }

    pub fn credential(&self) -> &ClientCredential {
        &self.credential
    }

    pub const fn responder(&self) -> Option<&ResponderName> {
        self.root_responder.as_ref()
    }
}

/// Not `Option<SessionAuthentication>`: declining authentication must be
/// spelled, so it cannot be reached by leaving a field blank.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ConnectionAuthentication {
    Unauthenticated,
    Session(SessionAuthentication),
}

impl ConnectionAuthentication {
    pub const fn session(&self) -> Option<&SessionAuthentication> {
        match self {
            Self::Unauthenticated => None,
            Self::Session(session) => Some(session),
        }
    }
}

impl From<SessionAuthentication> for ConnectionAuthentication {
    fn from(session: SessionAuthentication) -> Self {
        Self::Session(session)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectionConfig {
    pub address: String,
    pub uname: String,
    pub aname: String,
    pub msize: u32,
    pub authentication: ConnectionAuthentication,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_credential_and_a_responder_are_validated() {
        assert!(ClientCredential::new("/etc/r9p/client.conf").is_ok());
        assert!(ClientCredential::new("").is_err());
        assert!(ResponderName::new("terminal-m7").is_ok());
        assert!(ResponderName::new("").is_err());
    }

    #[test]
    fn a_contained_root_still_authenticates_its_referrals() {
        let credential = ClientCredential::new("/etc/r9p/client.conf").expect("credential");
        let session = SessionAuthentication::contained_root(credential);
        assert!(session.responder().is_none());

        let referred = session.for_responder(ResponderName::new("credentials").expect("responder"));
        assert_eq!(referred.credential(), session.credential());
        assert_eq!(
            referred.responder().map(ResponderName::as_str),
            Some("credentials")
        );
    }

    #[test]
    fn an_authenticated_root_carries_its_own_responder() {
        let credential = ClientCredential::new("/etc/r9p/client.conf").expect("credential");
        let session = SessionAuthentication::authenticated_root(
            credential,
            ResponderName::new("coordinator").expect("responder"),
        );
        assert_eq!(
            session.responder().map(ResponderName::as_str),
            Some("coordinator")
        );
    }
}
