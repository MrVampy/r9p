mod config;
mod handshake;
mod key;
mod p9any;
mod peercred;
mod stream;

pub use config::{ClientConfig, ServerConfig};
pub use handshake::{
    authenticate_client, authenticate_server, authenticate_server_attested, AuthenticatedSession,
    PeerIdentity,
};
pub use key::{
    generate_key_pair, provision_key_pair, write_key_pair, KeyPair, PrivateKey, PublicKey,
};
pub use peercred::{
    TransportIdentity, NOISE_SUBJECT_PREFIX, UNIX_PEER_SUBJECT_PREFIX, UNIX_SAME_USER_SUBJECT,
};
pub use stream::SecureStream;

pub const AUTH_PROTOCOL: &str = r9p::export_descriptor::P9ANY_NOISE_IK;
pub const CONFIG_FORMAT: &str = "r9p-session-auth.v1";
pub const SESSION_AUTH_SERVER_CONFIG_ENV: &str = "R9P_SESSION_AUTH_SERVER_CONFIG";

pub(crate) const NOISE_PATTERN: &str = "Noise_IK_25519_ChaChaPoly_BLAKE2s";
