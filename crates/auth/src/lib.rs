mod cert;
mod config;
mod handshake;
mod key;
mod p9any;
mod peercred;
mod stream;

pub use cert::{
    generate_root_key_pair, now_unix, provision_root_key_pair, Certificate, CertificateBody,
    RootKeyPair, RootPrivateKey, RootPublicKey, UnixSeconds, CERT_FORMAT,
};
pub use config::{ClientConfig, ServerConfig};
pub use handshake::{
    authenticate_client_to, authenticate_server, AuthenticatedSession, AuthenticationTimeouts,
    AuthenticationTransport, PeerIdentity,
};
pub use key::{
    generate_key_pair, provision_key_pair, write_key_pair, KeyPair, PrivateKey, PublicKey,
};
pub use peercred::{
    TransportIdentity, CERT_SUBJECT_PREFIX, UNIX_PEER_SUBJECT_PREFIX, UNIX_SAME_USER_SUBJECT,
};
pub use stream::SecureStream;

pub const AUTH_PROTOCOL_XX: &str = r9p::export_descriptor::P9ANY_NOISE_XX;
pub const CONFIG_FORMAT: &str = "r9p-session-auth.v1";
pub const SESSION_AUTH_SERVER_CONFIG_ENV: &str = "R9P_SESSION_AUTH_SERVER_CONFIG";

/// XX transmits both statics inside the handshake, each encrypted, so neither
/// side needs the other's key beforehand and neither leaks it to an observer.
pub(crate) const NOISE_PATTERN_XX: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
