use crate::{
    cert::{now_unix, Certificate},
    config::validate_principal,
    p9any,
    p9any::Protocol,
    ClientConfig, PublicKey, SecureStream, ServerConfig, CONFIG_FORMAT, NOISE_PATTERN,
};
use r9p::{
    error::{Error, Result},
    multiplex::MultiplexTransport,
    server::ConnectionStream,
};
use snow::{params::NoiseParams, HandshakeState};
use std::{
    collections::BTreeSet,
    io::{Read, Write},
    net::TcpStream,
    time::Duration,
};

const MAX_NOISE_MESSAGE_BYTES: usize = u16::MAX as usize;
const MAX_CERTIFICATE_BYTES: usize = 8192;
const SERVER_ACK: &[u8] = b"r9p-session-authenticated.v1";

/// Leading byte marking the first handshake payload as a certificate rather
/// than a bare principal. `validate_principal` rejects NUL and every other
/// control byte, so a legacy payload can never begin with this and the two
/// forms need no version negotiation to tell apart.
const CERTIFICATE_TAG: u8 = 0x00;

mod sealed {
    pub trait Sealed {}
}

#[doc(hidden)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthenticationTimeouts {
    read: Option<Duration>,
    write: Option<Duration>,
}

pub trait AuthenticationTransport: ConnectionStream + MultiplexTransport + sealed::Sealed {
    #[doc(hidden)]
    fn configure_authentication_transport(&self) -> Result<()>;

    #[doc(hidden)]
    fn install_authentication_timeout(&self, timeout: Duration) -> Result<AuthenticationTimeouts>;

    #[doc(hidden)]
    fn restore_authentication_timeouts(&self, timeouts: AuthenticationTimeouts) -> Result<()>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerIdentity {
    principal: String,
    public_key: PublicKey,
    groups: BTreeSet<String>,
    certified: bool,
}

pub struct AuthenticatedSession<S = TcpStream> {
    pub stream: SecureStream<S>,
    pub peer: PeerIdentity,
}

impl PeerIdentity {
    pub fn new(principal: impl Into<String>, public_key: PublicKey) -> Result<Self> {
        let principal = principal.into();
        validate_principal(&principal)?;
        Ok(Self {
            principal,
            public_key,
            groups: BTreeSet::new(),
            certified: false,
        })
    }

    pub fn principal(&self) -> &str {
        &self.principal
    }

    pub const fn public_key(&self) -> PublicKey {
        self.public_key
    }

    /// Groups carried by the certificate that named this peer. Always empty for
    /// a peer-line identity, so authorization written against groups denies a
    /// legacy session rather than silently widening it.
    pub fn groups(&self) -> impl Iterator<Item = &str> {
        self.groups.iter().map(String::as_str)
    }

    pub fn in_group(&self, group: &str) -> bool {
        self.groups.contains(group)
    }

    /// Whether the name was read from signed material rather than asserted by
    /// this server's own configuration.
    pub const fn certified(&self) -> bool {
        self.certified
    }
}

/// Authenticates using the domain named in the config.
///
/// Prefer [`authenticate_client_to`]: a certified client carries no domain, and
/// supplying it per connection is what lets one config serve every service.
pub fn authenticate_client<S: AuthenticationTransport>(
    stream: S,
    config: &ClientConfig,
    principal: &str,
    timeout: Duration,
) -> Result<SecureStream<S>> {
    let domain = config
        .domain()
        .ok_or_else(|| {
            Error::from("client session auth config has no domain; use authenticate_client_to")
        })?
        .to_string();
    authenticate_client_to(stream, config, &domain, principal, timeout)
}

/// Authenticates to `domain`, which is also the responder name the certificate
/// must carry. That check is what stops a differently-named service holding a
/// valid certificate from answering in another's place.
pub fn authenticate_client_to<S: AuthenticationTransport>(
    mut stream: S,
    config: &ClientConfig,
    domain: &str,
    principal: &str,
    timeout: Duration,
) -> Result<SecureStream<S>> {
    stream.configure_authentication_transport()?;
    validate_principal(principal)?;
    let previous = stream.install_authentication_timeout(timeout)?;
    let protocol = p9any::negotiate_client(&mut stream, domain, config.can_verify_responder())?;
    let prologue = prologue(domain);

    let transport = match protocol {
        Protocol::NoiseXx => client_xx(&mut stream, config, domain, principal, &prologue)?,
        // Rollout scaffolding. Deleted once every service speaks XX.
        Protocol::NoiseIk => client_ik(&mut stream, config, principal, &prologue)?,
    };
    stream.restore_authentication_timeouts(previous)?;
    Ok(SecureStream::new(stream, transport))
}

/// XX: the responder's static arrives in message two, carrying its certificate,
/// and this side's static goes out in message three with its own. Neither key
/// is configured in advance and neither is visible to an observer.
fn client_xx<S: AuthenticationTransport>(
    stream: &mut S,
    config: &ClientConfig,
    domain: &str,
    principal: &str,
    prologue: &[u8],
) -> Result<snow::StatelessTransportState> {
    let certificate = config
        .certificate()
        .ok_or_else(|| Error::from("XX requires a client certificate"))?;
    if certificate.body().name() != principal {
        return Err(Error::from(format!(
            "certificate names {} but the session asked for {principal}",
            certificate.body().name()
        )));
    }
    let mut handshake = snow::Builder::new(noise_params_xx()?)
        .prologue(prologue)
        .map_err(noise_error("configure authentication domain"))?
        .local_private_key(config.private_key().as_bytes())
        .map_err(noise_error("configure client private key"))?
        .build_initiator()
        .map_err(noise_error("create Noise initiator"))?;

    let mut first = vec![0_u8; MAX_NOISE_MESSAGE_BYTES];
    let first_len = handshake
        .write_message(&[], &mut first)
        .map_err(noise_error("write client hello"))?;
    write_frame(stream, &first[..first_len])?;

    let second = read_frame(stream, "server identity message")?;
    let mut payload = vec![0_u8; MAX_NOISE_MESSAGE_BYTES];
    let payload_len = handshake
        .read_message(&second, &mut payload)
        .map_err(noise_error("read server identity message"))?;
    let responder_key = PublicKey::from_bytes(
        handshake
            .get_remote_static()
            .ok_or_else(|| Error::from("Noise handshake did not carry a server key"))?,
    )?;
    verify_responder(config, &payload[..payload_len], responder_key, domain)?;

    let rendered = certificate.render();
    let mut third = vec![0_u8; MAX_NOISE_MESSAGE_BYTES];
    let third_len = handshake
        .write_message(rendered.as_bytes(), &mut third)
        .map_err(noise_error("write client identity message"))?;
    write_frame(stream, &third[..third_len])?;
    finish_handshake(handshake)
}

/// Authenticates the responder from its certificate: signed by a trusted root,
/// issued for the key that just completed the handshake, and named `domain`.
fn verify_responder(
    config: &ClientConfig,
    body: &[u8],
    responder_key: PublicKey,
    domain: &str,
) -> Result<()> {
    if body.len() > MAX_CERTIFICATE_BYTES {
        return Err(Error::from(format!(
            "responder certificate exceeds {MAX_CERTIFICATE_BYTES} bytes"
        )));
    }
    let text =
        std::str::from_utf8(body).map_err(|_| Error::from("responder certificate is not utf-8"))?;
    let certificate = Certificate::parse(text)?;
    if certificate.body().key() != responder_key {
        return Err(Error::from(format!(
            "responder certificate for {} was issued for a different key",
            certificate.body().name()
        )));
    }
    if certificate.body().name() != domain {
        return Err(Error::from(format!(
            "asked for {domain} but the responder proved it is {}",
            certificate.body().name()
        )));
    }
    let now = now_unix()?;
    let mut refusal = None;
    for root in config.roots() {
        match certificate.verify(*root, now) {
            Ok(()) => return Ok(()),
            Err(error) => refusal = Some(error),
        }
    }
    Err(refusal.unwrap_or_else(|| Error::from("responder certificate was not accepted")))
}

/// Rollout scaffolding: the pinned-responder path. Deleted with the last
/// `server-key` line.
fn client_ik<S: AuthenticationTransport>(
    stream: &mut S,
    config: &ClientConfig,
    principal: &str,
    prologue: &[u8],
) -> Result<snow::StatelessTransportState> {
    let server_key = config
        .server_key()
        .ok_or_else(|| Error::from("IK requires a pinned server-key"))?;
    let mut handshake = snow::Builder::new(noise_params()?)
        .prologue(prologue)
        .map_err(noise_error("configure authentication domain"))?
        .local_private_key(config.private_key().as_bytes())
        .map_err(noise_error("configure client private key"))?
        .remote_public_key(server_key.as_bytes())
        .map_err(noise_error("configure server public key"))?
        .build_initiator()
        .map_err(noise_error("create Noise initiator"))?;

    let payload = match config.certificate() {
        Some(certificate) => {
            if certificate.body().name() != principal {
                return Err(Error::from(format!(
                    "certificate names {} but the session asked for {principal}",
                    certificate.body().name()
                )));
            }
            let rendered = certificate.render();
            let mut payload = Vec::with_capacity(rendered.len() + 1);
            payload.push(CERTIFICATE_TAG);
            payload.extend_from_slice(rendered.as_bytes());
            payload
        }
        None => principal.as_bytes().to_vec(),
    };

    let mut first = vec![0_u8; MAX_NOISE_MESSAGE_BYTES];
    let first_len = handshake
        .write_message(&payload, &mut first)
        .map_err(noise_error("write client authentication message"))?;
    write_frame(stream, &first[..first_len])?;

    let second = read_frame(stream, "server authentication message")?;
    let mut response = vec![0_u8; second.len()];
    let response_len = handshake
        .read_message(&second, &mut response)
        .map_err(noise_error("read server authentication message"))?;
    if &response[..response_len] != SERVER_ACK {
        return Err(Error::from(
            "server authentication acknowledgement is invalid",
        ));
    }
    finish_handshake(handshake)
}

pub fn authenticate_server<S: AuthenticationTransport>(
    mut stream: S,
    config: &ServerConfig,
    timeout: Duration,
) -> Result<AuthenticatedSession<S>> {
    stream.configure_authentication_transport()?;
    let previous = stream.install_authentication_timeout(timeout)?;
    let protocol =
        p9any::negotiate_server(&mut stream, config.domain(), config.can_prove_identity())?;
    let prologue = prologue(config.domain());

    let (peer, transport) = match protocol {
        Protocol::NoiseXx => server_xx(&mut stream, config, &prologue)?,
        // Rollout scaffolding. Deleted once every client speaks XX.
        Protocol::NoiseIk => server_ik(&mut stream, config, &prologue)?,
    };
    stream.restore_authentication_timeouts(previous)?;
    Ok(AuthenticatedSession {
        stream: SecureStream::new(stream, transport),
        peer,
    })
}

/// XX responder: prove this service's identity in message two before the client
/// reveals its own in message three. Both statics stay encrypted.
fn server_xx<S: AuthenticationTransport>(
    stream: &mut S,
    config: &ServerConfig,
    prologue: &[u8],
) -> Result<(PeerIdentity, snow::StatelessTransportState)> {
    let own = config
        .certificate()
        .ok_or_else(|| Error::from("XX requires this server to hold a certificate"))?;
    let mut handshake = snow::Builder::new(noise_params_xx()?)
        .prologue(prologue)
        .map_err(noise_error("configure authentication domain"))?
        .local_private_key(config.private_key().as_bytes())
        .map_err(noise_error("configure server private key"))?
        .build_responder()
        .map_err(noise_error("create Noise responder"))?;

    let first = read_frame(stream, "client hello")?;
    let mut discard = vec![0_u8; MAX_NOISE_MESSAGE_BYTES];
    handshake
        .read_message(&first, &mut discard)
        .map_err(noise_error("read client hello"))?;

    let rendered = own.render();
    let mut second = vec![0_u8; MAX_NOISE_MESSAGE_BYTES];
    let second_len = handshake
        .write_message(rendered.as_bytes(), &mut second)
        .map_err(noise_error("write server identity message"))?;
    write_frame(stream, &second[..second_len])?;

    let third = read_frame(stream, "client identity message")?;
    let mut payload = vec![0_u8; MAX_NOISE_MESSAGE_BYTES];
    let payload_len = handshake
        .read_message(&third, &mut payload)
        .map_err(noise_error("read client identity message"))?;
    let public_key = PublicKey::from_bytes(
        handshake
            .get_remote_static()
            .ok_or_else(|| Error::from("Noise handshake did not authenticate a client key"))?,
    )?;
    // No bare-principal path here: XX is certificates only, by construction.
    let identity = certified_identity(config, &payload[..payload_len], public_key)?;
    Ok((identity, finish_handshake(handshake)?))
}

/// Rollout scaffolding: peer lines and pinned responders. Deleted with them.
fn server_ik<S: AuthenticationTransport>(
    stream: &mut S,
    config: &ServerConfig,
    prologue: &[u8],
) -> Result<(PeerIdentity, snow::StatelessTransportState)> {
    let mut handshake = snow::Builder::new(noise_params()?)
        .prologue(prologue)
        .map_err(noise_error("configure authentication domain"))?
        .local_private_key(config.private_key().as_bytes())
        .map_err(noise_error("configure server private key"))?
        .build_responder()
        .map_err(noise_error("create Noise responder"))?;

    let first = read_frame(stream, "client authentication message")?;
    let mut payload_bytes = vec![0_u8; MAX_NOISE_MESSAGE_BYTES];
    let payload_len = handshake
        .read_message(&first, &mut payload_bytes)
        .map_err(noise_error("read client authentication message"))?;
    let payload = &payload_bytes[..payload_len];
    let public_key = PublicKey::from_bytes(
        handshake
            .get_remote_static()
            .ok_or_else(|| Error::from("Noise handshake did not authenticate a client key"))?,
    )?;

    let identity = if payload.first() == Some(&CERTIFICATE_TAG) {
        certified_identity(config, &payload[1..], public_key)?
    } else {
        let principal = String::from_utf8(payload.to_vec())
            .map_err(|_| Error::from("session principal is not utf-8"))?;
        validate_principal(&principal)?;
        if !config.authorize(public_key, &principal) {
            return Err(Error::from(format!(
                "client key is not authorized for principal {principal}"
            )));
        }
        PeerIdentity {
            principal,
            public_key,
            groups: BTreeSet::new(),
            certified: false,
        }
    };

    let mut second = vec![0_u8; MAX_NOISE_MESSAGE_BYTES];
    let second_len = handshake
        .write_message(SERVER_ACK, &mut second)
        .map_err(noise_error("write server authentication message"))?;
    write_frame(stream, &second[..second_len])?;
    Ok((identity, finish_handshake(handshake)?))
}

/// Admits a certificate-bearing client.
///
/// The load-bearing check is that the certificate was issued for the key that
/// just completed this handshake. Certificates are public, so without it anyone
/// could replay another principal's certificate and be believed. Noise proves
/// possession of the key; this ties the signed name to that same key.
fn certified_identity(
    config: &ServerConfig,
    body: &[u8],
    public_key: PublicKey,
) -> Result<PeerIdentity> {
    if config.roots().is_empty() {
        return Err(Error::from(
            "client presented a certificate but this server accepts none",
        ));
    }
    if body.len() > MAX_CERTIFICATE_BYTES {
        return Err(Error::from(format!(
            "presented certificate exceeds {MAX_CERTIFICATE_BYTES} bytes"
        )));
    }
    let text =
        std::str::from_utf8(body).map_err(|_| Error::from("presented certificate is not utf-8"))?;
    let certificate = Certificate::parse(text)?;
    if certificate.body().key() != public_key {
        return Err(Error::from(format!(
            "certificate for {} was issued for a different session key",
            certificate.body().name()
        )));
    }

    let now = now_unix()?;
    let mut refusal = None;
    for root in config.roots() {
        match certificate.verify(*root, now) {
            Ok(()) => {
                return Ok(PeerIdentity {
                    principal: certificate.body().name().to_string(),
                    public_key,
                    groups: certificate.body().groups().map(str::to_string).collect(),
                    certified: true,
                });
            }
            Err(error) => refusal = Some(error),
        }
    }
    Err(refusal.unwrap_or_else(|| Error::from("presented certificate was not accepted")))
}

fn noise_params_xx() -> Result<NoiseParams> {
    crate::NOISE_PATTERN_XX
        .parse()
        .map_err(|error| Error::from(format!("parse Noise XX pattern: {error}")))
}

fn noise_params() -> Result<NoiseParams> {
    NOISE_PATTERN
        .parse()
        .map_err(|error| Error::from(format!("parse Noise pattern: {error}")))
}

fn prologue(domain: &str) -> Vec<u8> {
    let mut prologue = Vec::with_capacity(CONFIG_FORMAT.len() + domain.len() + 1);
    prologue.extend_from_slice(CONFIG_FORMAT.as_bytes());
    prologue.push(0);
    prologue.extend_from_slice(domain.as_bytes());
    prologue
}

fn finish_handshake(handshake: HandshakeState) -> Result<snow::StatelessTransportState> {
    if !handshake.is_handshake_finished() {
        return Err(Error::from("Noise authentication handshake is incomplete"));
    }
    handshake
        .into_stateless_transport_mode()
        .map_err(noise_error("enter encrypted transport mode"))
}

fn write_frame<S: Write>(stream: &mut S, message: &[u8]) -> Result<()> {
    let length = u16::try_from(message.len())
        .map_err(|_| Error::from("Noise handshake message exceeds framing limit"))?;
    stream
        .write_all(&length.to_be_bytes())
        .and_then(|()| stream.write_all(message))
        .and_then(|()| stream.flush())
        .map_err(|error| Error::from(format!("write authentication message: {error}")))
}

fn read_frame<S: Read>(stream: &mut S, label: &str) -> Result<Vec<u8>> {
    let mut encoded_len = [0_u8; 2];
    stream
        .read_exact(&mut encoded_len)
        .map_err(|error| Error::from(format!("read {label} length: {error}")))?;
    let length = usize::from(u16::from_be_bytes(encoded_len));
    if length == 0 {
        return Err(Error::from(format!("{label} is empty")));
    }
    let mut message = vec![0_u8; length];
    stream
        .read_exact(&mut message)
        .map_err(|error| Error::from(format!("read {label}: {error}")))?;
    Ok(message)
}

impl sealed::Sealed for TcpStream {}

impl AuthenticationTransport for TcpStream {
    fn configure_authentication_transport(&self) -> Result<()> {
        self.set_nodelay(true)
            .map_err(|error| Error::from(format!("set TCP_NODELAY: {error}")))
    }

    fn install_authentication_timeout(&self, timeout: Duration) -> Result<AuthenticationTimeouts> {
        if timeout.is_zero() {
            return Err(Error::from(
                "authentication handshake timeout must be nonzero",
            ));
        }
        let previous = AuthenticationTimeouts {
            read: self
                .read_timeout()
                .map_err(|error| Error::from(format!("read TCP timeout: {error}")))?,
            write: self
                .write_timeout()
                .map_err(|error| Error::from(format!("read TCP timeout: {error}")))?,
        };
        self.set_read_timeout(Some(timeout))
            .and_then(|()| self.set_write_timeout(Some(timeout)))
            .map_err(|error| {
                Error::from(format!("set authentication handshake timeout: {error}"))
            })?;
        Ok(previous)
    }

    fn restore_authentication_timeouts(&self, timeouts: AuthenticationTimeouts) -> Result<()> {
        self.set_read_timeout(timeouts.read)
            .and_then(|()| self.set_write_timeout(timeouts.write))
            .map_err(|error| Error::from(format!("restore TCP timeout: {error}")))
    }
}

impl<S: AuthenticationTransport> sealed::Sealed for SecureStream<S> {}

impl<S: AuthenticationTransport> AuthenticationTransport for SecureStream<S> {
    fn configure_authentication_transport(&self) -> Result<()> {
        self.transport_stream().configure_authentication_transport()
    }

    fn install_authentication_timeout(&self, timeout: Duration) -> Result<AuthenticationTimeouts> {
        self.transport_stream()
            .install_authentication_timeout(timeout)
    }

    fn restore_authentication_timeouts(&self, timeouts: AuthenticationTimeouts) -> Result<()> {
        self.transport_stream()
            .restore_authentication_timeouts(timeouts)
    }
}

fn noise_error(context: &'static str) -> impl FnOnce(snow::Error) -> Error {
    move |error| Error::from(format!("{context}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate_key_pair;
    use std::{net::TcpListener, thread};

    #[test]
    fn authorized_principal_gets_an_encrypted_bidirectional_stream() -> Result<()> {
        let server_key = generate_key_pair()?;
        let client_key = generate_key_pair()?;
        let server_config = ServerConfig::new(
            "vault",
            server_key.private.clone(),
            [(client_key.public, "codex".to_string())],
        )?;
        let client_config = ClientConfig::new("vault", client_key.private, server_key.public)?;
        let listener =
            TcpListener::bind("127.0.0.1:0").map_err(|error| Error::from(error.to_string()))?;
        let address = listener
            .local_addr()
            .map_err(|error| Error::from(error.to_string()))?;
        let server = thread::spawn(move || -> Result<PeerIdentity> {
            let (stream, _) = listener
                .accept()
                .map_err(|error| Error::from(error.to_string()))?;
            let socket = stream
                .try_clone()
                .map_err(|error| Error::from(error.to_string()))?;
            let mut session = authenticate_server(stream, &server_config, Duration::from_secs(2))?;
            if !socket
                .nodelay()
                .map_err(|error| Error::from(error.to_string()))?
            {
                return Err(Error::from("authenticated server socket has Nagle enabled"));
            }
            let mut request = [0_u8; 5];
            session
                .stream
                .read_exact(&mut request)
                .map_err(|error| Error::from(error.to_string()))?;
            if request != *b"hello" {
                return Err(Error::from("server received wrong encrypted bytes"));
            }
            session
                .stream
                .write_all(b"world")
                .map_err(|error| Error::from(error.to_string()))?;
            Ok(session.peer)
        });
        let stream = TcpStream::connect(address).map_err(|error| Error::from(error.to_string()))?;
        let socket = stream
            .try_clone()
            .map_err(|error| Error::from(error.to_string()))?;
        let mut stream =
            authenticate_client(stream, &client_config, "codex", Duration::from_secs(2))?;
        assert!(socket
            .nodelay()
            .map_err(|error| Error::from(error.to_string()))?);
        stream
            .write_all(b"hello")
            .map_err(|error| Error::from(error.to_string()))?;
        let mut response = [0_u8; 5];
        stream
            .read_exact(&mut response)
            .map_err(|error| Error::from(error.to_string()))?;
        assert_eq!(&response, b"world");
        let identity = server
            .join()
            .map_err(|_| Error::from("auth server panicked"))??;
        assert_eq!(identity.principal(), "codex");
        assert_eq!(identity.public_key(), client_key.public);
        Ok(())
    }

    #[test]
    fn authorized_key_cannot_claim_a_different_principal() -> Result<()> {
        let server_key = generate_key_pair()?;
        let client_key = generate_key_pair()?;
        let server_config = ServerConfig::new(
            "vault",
            server_key.private.clone(),
            [(client_key.public, "codex".to_string())],
        )?;
        let client_config = ClientConfig::new("vault", client_key.private, server_key.public)?;
        let listener =
            TcpListener::bind("127.0.0.1:0").map_err(|error| Error::from(error.to_string()))?;
        let address = listener
            .local_addr()
            .map_err(|error| Error::from(error.to_string()))?;
        let server = thread::spawn(move || {
            let (stream, _) = listener
                .accept()
                .map_err(|error| Error::from(error.to_string()))?;
            authenticate_server(stream, &server_config, Duration::from_secs(2))
        });
        let stream = TcpStream::connect(address).map_err(|error| Error::from(error.to_string()))?;
        assert!(
            authenticate_client(stream, &client_config, "root", Duration::from_secs(2)).is_err()
        );
        assert!(server
            .join()
            .map_err(|_| Error::from("auth server panicked"))?
            .is_err());
        Ok(())
    }

    #[test]
    fn client_rejects_a_server_that_does_not_match_its_pinned_key() -> Result<()> {
        let server_key = generate_key_pair()?;
        let other_server_key = generate_key_pair()?;
        let client_key = generate_key_pair()?;
        let server_config = ServerConfig::new(
            "vault",
            server_key.private,
            [(client_key.public, "codex".to_string())],
        )?;
        let client_config =
            ClientConfig::new("vault", client_key.private, other_server_key.public)?;
        let listener =
            TcpListener::bind("127.0.0.1:0").map_err(|error| Error::from(error.to_string()))?;
        let address = listener
            .local_addr()
            .map_err(|error| Error::from(error.to_string()))?;
        let server = thread::spawn(move || {
            let (stream, _) = listener
                .accept()
                .map_err(|error| Error::from(error.to_string()))?;
            authenticate_server(stream, &server_config, Duration::from_secs(2))
        });
        let stream = TcpStream::connect(address).map_err(|error| Error::from(error.to_string()))?;
        assert!(
            authenticate_client(stream, &client_config, "codex", Duration::from_secs(2)).is_err()
        );
        assert!(server
            .join()
            .map_err(|_| Error::from("auth server panicked"))?
            .is_err());
        Ok(())
    }

    // ---- certificate-bearing sessions -------------------------------------

    use crate::cert::{generate_root_key_pair, CertificateBody, RootKeyPair};
    use crate::{KeyPair, PrivateKey};

    const VALID_FROM: crate::UnixSeconds = 1_000;
    const VALID_UNTIL: crate::UnixSeconds = 4_000_000_000;

    fn issue(
        root: &RootKeyPair,
        subject: PublicKey,
        name: &str,
        groups: &[&str],
        not_before: crate::UnixSeconds,
        not_after: crate::UnixSeconds,
    ) -> Result<Certificate> {
        let body = CertificateBody::new(
            name,
            subject,
            groups.iter().map(|group| (*group).to_string()),
            not_before,
            not_after,
            root.public,
        )?;
        Certificate::sign(&root.private, body)
    }

    /// Runs one handshake and returns the server's view. The server is joined
    /// before the client error is surfaced so a failure on either side is
    /// reported rather than deadlocking the test.
    fn handshake_once(
        server_config: ServerConfig,
        client_config: &ClientConfig,
        principal: &str,
    ) -> Result<PeerIdentity> {
        let listener =
            TcpListener::bind("127.0.0.1:0").map_err(|error| Error::from(error.to_string()))?;
        let address = listener
            .local_addr()
            .map_err(|error| Error::from(error.to_string()))?;
        let server = thread::spawn(move || -> Result<PeerIdentity> {
            let (stream, _) = listener
                .accept()
                .map_err(|error| Error::from(error.to_string()))?;
            let session = authenticate_server(stream, &server_config, Duration::from_secs(2))?;
            Ok(session.peer)
        });
        let stream = TcpStream::connect(address).map_err(|error| Error::from(error.to_string()))?;
        let client = authenticate_client(stream, client_config, principal, Duration::from_secs(2));
        let peer = server
            .join()
            .map_err(|_| Error::from("auth server panicked"))?;
        client?;
        peer
    }

    fn cert_client(
        server_key: &KeyPair,
        client_private: PrivateKey,
        certificate: Certificate,
    ) -> Result<ClientConfig> {
        ClientConfig::new("vault", client_private, server_key.public)?.with_certificate(certificate)
    }

    #[test]
    fn a_certificate_names_the_session_with_no_peer_line() -> Result<()> {
        let root = generate_root_key_pair()?;
        let server_key = generate_key_pair()?;
        let client_key = generate_key_pair()?;
        let certificate = issue(
            &root,
            client_key.public,
            "tuxedo",
            &["operator", "laptop"],
            VALID_FROM,
            VALID_UNTIL,
        )?;
        let server_config = ServerConfig::new_with_roots(
            "vault",
            server_key.private.clone(),
            Vec::new(),
            [root.public],
        )?;
        let client_config = cert_client(&server_key, client_key.private, certificate)?;

        let peer = handshake_once(server_config, &client_config, "tuxedo")?;
        assert_eq!(peer.principal(), "tuxedo");
        assert_eq!(peer.public_key(), client_key.public);
        assert!(peer.certified());
        assert!(peer.in_group("operator"));
        assert!(peer.in_group("laptop"));
        assert!(!peer.in_group("server"));
        Ok(())
    }

    #[test]
    fn a_certificate_cannot_be_replayed_by_another_key() -> Result<()> {
        // Certificates are public. Without binding the certificate to the key
        // that completed the handshake, anyone holding a copy could claim it.
        let root = generate_root_key_pair()?;
        let server_key = generate_key_pair()?;
        let owner = generate_key_pair()?;
        let thief = generate_key_pair()?;
        let certificate = issue(
            &root,
            owner.public,
            "tuxedo",
            &["operator"],
            VALID_FROM,
            VALID_UNTIL,
        )?;
        let server_config = ServerConfig::new_with_roots(
            "vault",
            server_key.private.clone(),
            Vec::new(),
            [root.public],
        )?;
        // with_certificate refuses the mismatch locally, so the stolen
        // certificate is forced past it to prove the server also refuses.
        let mut client_config = ClientConfig::new("vault", thief.private, server_key.public)?;
        client_config.force_certificate_for_test(certificate);
        assert!(handshake_once(server_config, &client_config, "tuxedo").is_err());
        Ok(())
    }

    #[test]
    fn a_certificate_from_an_unknown_root_is_refused() -> Result<()> {
        let root = generate_root_key_pair()?;
        let other = generate_root_key_pair()?;
        let server_key = generate_key_pair()?;
        let client_key = generate_key_pair()?;
        let certificate = issue(
            &other,
            client_key.public,
            "tuxedo",
            &[],
            VALID_FROM,
            VALID_UNTIL,
        )?;
        let server_config = ServerConfig::new_with_roots(
            "vault",
            server_key.private.clone(),
            Vec::new(),
            [root.public],
        )?;
        let client_config = cert_client(&server_key, client_key.private, certificate)?;
        assert!(handshake_once(server_config, &client_config, "tuxedo").is_err());
        Ok(())
    }

    #[test]
    fn an_expired_certificate_is_refused() -> Result<()> {
        let root = generate_root_key_pair()?;
        let server_key = generate_key_pair()?;
        let client_key = generate_key_pair()?;
        let certificate = issue(&root, client_key.public, "tuxedo", &[], 1_000, 2_000)?;
        let server_config = ServerConfig::new_with_roots(
            "vault",
            server_key.private.clone(),
            Vec::new(),
            [root.public],
        )?;
        let client_config = cert_client(&server_key, client_key.private, certificate)?;
        assert!(handshake_once(server_config, &client_config, "tuxedo").is_err());
        Ok(())
    }

    #[test]
    fn a_server_without_roots_refuses_a_certificate() -> Result<()> {
        let root = generate_root_key_pair()?;
        let server_key = generate_key_pair()?;
        let client_key = generate_key_pair()?;
        let certificate = issue(
            &root,
            client_key.public,
            "tuxedo",
            &[],
            VALID_FROM,
            VALID_UNTIL,
        )?;
        let server_config = ServerConfig::new(
            "vault",
            server_key.private.clone(),
            [(client_key.public, "tuxedo".to_string())],
        )?;
        let client_config = cert_client(&server_key, client_key.private, certificate)?;
        assert!(handshake_once(server_config, &client_config, "tuxedo").is_err());
        Ok(())
    }

    #[test]
    fn peer_lines_keep_working_while_roots_are_configured() -> Result<()> {
        // The migration state: a server trusting a root must not stop admitting
        // the clients that have not been cut over yet.
        let root = generate_root_key_pair()?;
        let server_key = generate_key_pair()?;
        let legacy = generate_key_pair()?;
        let server_config = ServerConfig::new_with_roots(
            "vault",
            server_key.private.clone(),
            [(legacy.public, "codex".to_string())],
            [root.public],
        )?;
        let client_config = ClientConfig::new("vault", legacy.private, server_key.public)?;
        let peer = handshake_once(server_config, &client_config, "codex")?;
        assert_eq!(peer.principal(), "codex");
        assert!(!peer.certified());
        assert_eq!(peer.groups().count(), 0);
        Ok(())
    }

    #[test]
    fn a_client_will_not_authenticate_under_a_name_it_was_not_asked_for() -> Result<()> {
        let root = generate_root_key_pair()?;
        let server_key = generate_key_pair()?;
        let client_key = generate_key_pair()?;
        let certificate = issue(
            &root,
            client_key.public,
            "tuxedo",
            &[],
            VALID_FROM,
            VALID_UNTIL,
        )?;
        let client_config = cert_client(&server_key, client_key.private, certificate)?;
        let server_config = ServerConfig::new_with_roots(
            "vault",
            server_key.private.clone(),
            Vec::new(),
            [root.public],
        )?;
        assert!(handshake_once(server_config, &client_config, "m7").is_err());
        Ok(())
    }

    #[test]
    fn a_certificate_issued_for_another_key_is_refused_before_use() -> Result<()> {
        let root = generate_root_key_pair()?;
        let server_key = generate_key_pair()?;
        let client_key = generate_key_pair()?;
        let other = generate_key_pair()?;
        let certificate = issue(&root, other.public, "tuxedo", &[], VALID_FROM, VALID_UNTIL)?;
        assert!(cert_client(&server_key, client_key.private, certificate).is_err());
        Ok(())
    }

    #[test]
    fn an_undersized_server_buffer_refuses_rather_than_truncating() -> Result<()> {
        // A server predating certificates reads the first payload into a
        // 255-byte buffer. A certificate does not fit. This must fail outright:
        // if it truncated instead, an old server could read a prefix of signed
        // material as a principal. That safe failure is what makes
        // servers-before-clients the correct migration order.
        let server_key = generate_key_pair()?;
        let client_key = generate_key_pair()?;
        let prologue = prologue("vault");
        let mut initiator = snow::Builder::new(noise_params()?)
            .prologue(&prologue)
            .map_err(noise_error("prologue"))?
            .local_private_key(client_key.private.as_bytes())
            .map_err(noise_error("client key"))?
            .remote_public_key(server_key.public.as_bytes())
            .map_err(noise_error("server key"))?
            .build_initiator()
            .map_err(noise_error("initiator"))?;
        let mut responder = snow::Builder::new(noise_params()?)
            .prologue(&prologue)
            .map_err(noise_error("prologue"))?
            .local_private_key(server_key.private.as_bytes())
            .map_err(noise_error("server key"))?
            .build_responder()
            .map_err(noise_error("responder"))?;

        let payload = vec![b'x'; 1024];
        let mut message = vec![0_u8; MAX_NOISE_MESSAGE_BYTES];
        let len = initiator
            .write_message(&payload, &mut message)
            .map_err(noise_error("write"))?;

        let mut undersized = vec![0_u8; 255];
        assert!(responder
            .read_message(&message[..len], &mut undersized)
            .is_err());
        Ok(())
    }

    // ---- XX: mutual certificates, nothing pinned --------------------------

    fn xx_pair(
        server_name: &str,
        client_name: &str,
    ) -> Result<(RootKeyPair, ServerConfig, ClientConfig)> {
        let root = generate_root_key_pair()?;
        let server_key = generate_key_pair()?;
        let client_key = generate_key_pair()?;
        let server = ServerConfig::new_with_roots(
            server_name,
            server_key.private,
            Vec::new(),
            [root.public],
        )?
        .with_certificate(issue(
            &root,
            server_key.public,
            server_name,
            &[],
            VALID_FROM,
            VALID_UNTIL,
        )?)?;
        let client = ClientConfig::certified(
            client_key.private,
            issue(
                &root,
                client_key.public,
                client_name,
                &["operator"],
                VALID_FROM,
                VALID_UNTIL,
            )?,
            [root.public],
        )?;
        Ok((root, server, client))
    }

    fn xx_once(
        server_config: ServerConfig,
        client_config: &ClientConfig,
        domain: &str,
        principal: &str,
    ) -> Result<PeerIdentity> {
        let listener =
            TcpListener::bind("127.0.0.1:0").map_err(|error| Error::from(error.to_string()))?;
        let address = listener
            .local_addr()
            .map_err(|error| Error::from(error.to_string()))?;
        let server = thread::spawn(move || -> Result<PeerIdentity> {
            let (stream, _) = listener
                .accept()
                .map_err(|error| Error::from(error.to_string()))?;
            Ok(authenticate_server(stream, &server_config, Duration::from_secs(2))?.peer)
        });
        let stream = TcpStream::connect(address).map_err(|error| Error::from(error.to_string()))?;
        let client = authenticate_client_to(
            stream,
            client_config,
            domain,
            principal,
            Duration::from_secs(2),
        );
        let peer = server
            .join()
            .map_err(|_| Error::from("auth server panicked"))?;
        client?;
        peer
    }

    #[test]
    fn xx_authenticates_both_sides_with_nothing_pinned() -> Result<()> {
        let (_, server, client) = xx_pair("terminal-m7", "tuxedo.operator")?;
        assert!(
            client.server_key().is_none(),
            "client pinned a responder key"
        );
        assert!(client.domain().is_none(), "client named a service");
        let peer = xx_once(server, &client, "terminal-m7", "tuxedo.operator")?;
        assert_eq!(peer.principal(), "tuxedo.operator");
        assert!(peer.certified());
        assert!(peer.in_group("operator"));
        Ok(())
    }

    #[test]
    fn a_responder_cannot_answer_under_another_service_name() -> Result<()> {
        // The whole point of authenticating the responder: a service holding a
        // perfectly valid certificate must not be able to answer in another's
        // place just by advertising that name.
        let root = generate_root_key_pair()?;
        let server_key = generate_key_pair()?;
        let client_key = generate_key_pair()?;
        let mut server = ServerConfig::new_with_roots(
            "terminal-m7",
            server_key.private,
            Vec::new(),
            [root.public],
        )?;
        server.force_certificate_for_test(issue(
            &root,
            server_key.public,
            "terminal-nucbox",
            &[],
            VALID_FROM,
            VALID_UNTIL,
        )?);
        let client = ClientConfig::certified(
            client_key.private,
            issue(&root, client_key.public, "op", &[], VALID_FROM, VALID_UNTIL)?,
            [root.public],
        )?;
        assert!(xx_once(server, &client, "terminal-m7", "op").is_err());
        Ok(())
    }

    #[test]
    fn a_responder_certified_by_another_root_is_refused() -> Result<()> {
        let other = generate_root_key_pair()?;
        let (_, server, _) = xx_pair("terminal-m7", "op")?;
        let client_key = generate_key_pair()?;
        let client = ClientConfig::certified(
            client_key.private,
            issue(
                &other,
                client_key.public,
                "op",
                &[],
                VALID_FROM,
                VALID_UNTIL,
            )?,
            [other.public],
        )?;
        assert!(xx_once(server, &client, "terminal-m7", "op").is_err());
        Ok(())
    }

    #[test]
    fn xx_binds_the_client_certificate_to_its_key() -> Result<()> {
        let (root, server, _) = xx_pair("terminal-m7", "op")?;
        let thief = generate_key_pair()?;
        let owner = generate_key_pair()?;
        let stolen = issue(&root, owner.public, "op", &[], VALID_FROM, VALID_UNTIL)?;
        let mut client = ClientConfig::certified(
            thief.private,
            issue(&root, thief.public, "op", &[], VALID_FROM, VALID_UNTIL)?,
            [root.public],
        )?;
        client.force_certificate_for_test(stolen);
        assert!(xx_once(server, &client, "terminal-m7", "op").is_err());
        Ok(())
    }

    #[test]
    fn an_expired_responder_certificate_is_refused() -> Result<()> {
        let root = generate_root_key_pair()?;
        let server_key = generate_key_pair()?;
        let client_key = generate_key_pair()?;
        let mut server = ServerConfig::new_with_roots(
            "terminal-m7",
            server_key.private,
            Vec::new(),
            [root.public],
        )?;
        server.force_certificate_for_test(issue(
            &root,
            server_key.public,
            "terminal-m7",
            &[],
            1_000,
            2_000,
        )?);
        let client = ClientConfig::certified(
            client_key.private,
            issue(&root, client_key.public, "op", &[], VALID_FROM, VALID_UNTIL)?,
            [root.public],
        )?;
        assert!(xx_once(server, &client, "terminal-m7", "op").is_err());
        Ok(())
    }

    #[test]
    fn a_server_that_cannot_prove_itself_falls_back_rather_than_offering_xx() -> Result<()> {
        // Rollout state: an un-certified server still serves a certified client
        // over IK, which is what lets servers and clients migrate separately.
        let root = generate_root_key_pair()?;
        let server_key = generate_key_pair()?;
        let client_key = generate_key_pair()?;
        let server = ServerConfig::new_with_roots(
            "vault",
            server_key.private.clone(),
            Vec::new(),
            [root.public],
        )?;
        assert!(!server.can_prove_identity());
        let client = ClientConfig::new("vault", client_key.private, server_key.public)?
            .with_certificate(issue(
                &root,
                client_key.public,
                "op",
                &["operator"],
                VALID_FROM,
                VALID_UNTIL,
            )?)?;
        let peer = handshake_once(server, &client, "op")?;
        assert_eq!(peer.principal(), "op");
        assert!(peer.certified());
        Ok(())
    }
}
