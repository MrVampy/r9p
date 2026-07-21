mod config;
mod handshake;
mod key;
mod p9any;
mod stream;

pub use config::{ClientConfig, ServerConfig};
pub use handshake::{authenticate_client, authenticate_server, AuthenticatedSession, PeerIdentity};
pub use key::{generate_key_pair, write_key_pair, KeyPair, PrivateKey, PublicKey};
pub use stream::SecureStream;

pub const AUTH_CLASS: &str = "p9any";
pub const AUTH_PROTOCOL: &str = "noise-ik";
pub const CONFIG_FORMAT: &str = "r9p-session-auth.v1";

pub(crate) const NOISE_PATTERN: &str = "Noise_IK_25519_ChaChaPoly_BLAKE2s";
