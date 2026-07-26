mod config;
mod handshake;
mod key;
mod p9any;
mod peercred;
mod stream;

pub use config::{ClientConfig, ServerConfig};
pub use handshake::{authenticate_client, authenticate_server, AuthenticatedSession, PeerIdentity};
pub use key::{
    generate_key_pair, provision_key_pair, write_key_pair, KeyPair, PrivateKey, PublicKey,
};
pub use peercred::{PeerCredentialConfig, TransportIdentity};
pub use stream::SecureStream;

pub const AUTH_PROTOCOL: &str = r9p::export_descriptor::P9ANY_NOISE_IK;
pub const CONFIG_FORMAT: &str = "r9p-session-auth.v1";
pub const PEER_CREDENTIAL_CONFIG_FORMAT: &str = "r9p-peercred-auth.v1";
pub const SESSION_AUTH_SERVER_CONFIG_ENV: &str = "R9P_SESSION_AUTH_SERVER_CONFIG";
pub const PEER_CREDENTIAL_CONFIG_ENV: &str = "R9P_PEER_CREDENTIAL_CONFIG";

pub(crate) const NOISE_PATTERN: &str = "Noise_IK_25519_ChaChaPoly_BLAKE2s";
