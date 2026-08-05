use crate::{
    cert::{now_unix, Certificate},
    config::validate_principal,
    p9any,
    p9any::Protocol,
    ClientConfig, PublicKey, SecureStream, ServerConfig, CONFIG_FORMAT,
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
    p9any::negotiate_client(&mut stream, domain)?;
    let prologue = prologue(domain);
    let transport = client_xx(&mut stream, config, domain, principal, &prologue)?;
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

pub fn authenticate_server<S: AuthenticationTransport>(
    mut stream: S,
    config: &ServerConfig,
    timeout: Duration,
) -> Result<AuthenticatedSession<S>> {
    stream.configure_authentication_transport()?;
    let previous = stream.install_authentication_timeout(timeout)?;
    let protocol =
        p9any::negotiate_server(&mut stream, config.domain())?;
    let prologue = prologue(config.domain());
    let (peer, transport) = server_xx(&mut stream, config, &prologue)?;
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


    fn xx_pair(
        server_name: &str,
        client_name: &str,
    ) -> Result<(RootKeyPair, ServerConfig, ClientConfig)> {
        let root = generate_root_key_pair()?;
        let server_key = generate_key_pair()?;
        let client_key = generate_key_pair()?;
        let server = ServerConfig::new(server_name, server_key.private, [root.public])?
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
        let mut server = ServerConfig::new("terminal-m7", server_key.private, [root.public])?;
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
        let mut server = ServerConfig::new("terminal-m7", server_key.private, [root.public])?;
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
    fn a_client_certificate_from_an_unknown_root_is_refused() -> Result<()> {
        let (_root, server, _client) = xx_pair("vault", "op")?;
        let other = generate_root_key_pair()?;
        let client_key = generate_key_pair()?;
        let client = ClientConfig::certified(
            client_key.private.clone(),
            issue(
                &other,
                client_key.public,
                "op",
                &["operator"],
                VALID_FROM,
                VALID_UNTIL,
            )?,
            [other.public],
        )?;

        assert!(xx_once(server, &client, "vault", "op").is_err());
        Ok(())
    }

    #[test]
    fn an_expired_client_certificate_is_refused() -> Result<()> {
        let (root, server, _client) = xx_pair("vault", "op")?;
        let client_key = generate_key_pair()?;
        let client = ClientConfig::certified(
            client_key.private.clone(),
            issue(&root, client_key.public, "op", &["operator"], 1, 2)?,
            [root.public],
        )?;

        assert!(xx_once(server, &client, "vault", "op").is_err());
        Ok(())
    }

    #[test]
    fn a_server_without_roots_cannot_serve_xx() -> Result<()> {
        let root = generate_root_key_pair()?;
        let server_key = generate_key_pair()?;

        assert!(ServerConfig::new("vault", server_key.private.clone(), Vec::new()).is_err());
        assert!(ServerConfig::new("vault", server_key.private, [root.public]).is_ok());
        Ok(())
    }
}
