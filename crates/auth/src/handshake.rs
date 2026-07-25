use crate::{
    config::validate_principal, p9any, ClientConfig, PublicKey, SecureStream, ServerConfig,
    CONFIG_FORMAT, NOISE_PATTERN,
};
use r9p::error::{Error, Result};
use snow::{params::NoiseParams, HandshakeState};
use std::{
    io::{Read, Write},
    net::TcpStream,
    time::Duration,
};

const MAX_NOISE_MESSAGE_BYTES: usize = u16::MAX as usize;
const MAX_PRINCIPAL_BYTES: usize = 255;
const SERVER_ACK: &[u8] = b"r9p-session-authenticated.v1";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerIdentity {
    principal: String,
    public_key: PublicKey,
}

pub struct AuthenticatedSession {
    pub stream: SecureStream,
    pub peer: PeerIdentity,
}

impl PeerIdentity {
    pub fn principal(&self) -> &str {
        &self.principal
    }

    pub const fn public_key(&self) -> PublicKey {
        self.public_key
    }
}

pub fn authenticate_client(
    mut stream: TcpStream,
    config: &ClientConfig,
    principal: &str,
    timeout: Duration,
) -> Result<SecureStream> {
    configure_transport_socket(&stream)?;
    validate_principal(principal)?;
    let previous = install_handshake_timeout(&stream, timeout)?;
    p9any::negotiate_client(&mut stream, config.domain())?;

    let params = noise_params()?;
    let prologue = prologue(config.domain());
    let builder = snow::Builder::new(params)
        .prologue(&prologue)
        .map_err(noise_error("configure authentication domain"))?;
    let mut handshake = builder
        .local_private_key(config.private_key().as_bytes())
        .map_err(noise_error("configure client private key"))?
        .remote_public_key(config.server_key().as_bytes())
        .map_err(noise_error("configure server public key"))?
        .build_initiator()
        .map_err(noise_error("create Noise initiator"))?;

    let mut first = vec![0_u8; MAX_NOISE_MESSAGE_BYTES];
    let first_len = handshake
        .write_message(principal.as_bytes(), &mut first)
        .map_err(noise_error("write client authentication message"))?;
    write_frame(&mut stream, &first[..first_len])?;

    let second = read_frame(&mut stream, "server authentication message")?;
    let mut response = vec![0_u8; second.len()];
    let response_len = handshake
        .read_message(&second, &mut response)
        .map_err(noise_error("read server authentication message"))?;
    if &response[..response_len] != SERVER_ACK {
        return Err(Error::from(
            "server authentication acknowledgement is invalid",
        ));
    }
    let transport = finish_handshake(handshake)?;
    restore_timeouts(&stream, previous)?;
    Ok(SecureStream::new(stream, transport))
}

pub fn authenticate_server(
    mut stream: TcpStream,
    config: &ServerConfig,
    timeout: Duration,
) -> Result<AuthenticatedSession> {
    configure_transport_socket(&stream)?;
    let previous = install_handshake_timeout(&stream, timeout)?;
    p9any::negotiate_server(&mut stream, config.domain())?;

    let params = noise_params()?;
    let prologue = prologue(config.domain());
    let builder = snow::Builder::new(params)
        .prologue(&prologue)
        .map_err(noise_error("configure authentication domain"))?;
    let mut handshake = builder
        .local_private_key(config.private_key().as_bytes())
        .map_err(noise_error("configure server private key"))?
        .build_responder()
        .map_err(noise_error("create Noise responder"))?;

    let first = read_frame(&mut stream, "client authentication message")?;
    let mut principal_bytes = vec![0_u8; MAX_PRINCIPAL_BYTES];
    let principal_len = handshake
        .read_message(&first, &mut principal_bytes)
        .map_err(noise_error("read client authentication message"))?;
    let principal = String::from_utf8(principal_bytes[..principal_len].to_vec())
        .map_err(|_| Error::from("session principal is not utf-8"))?;
    validate_principal(&principal)?;
    let public_key = PublicKey::from_bytes(
        handshake
            .get_remote_static()
            .ok_or_else(|| Error::from("Noise handshake did not authenticate a client key"))?,
    )?;
    if !config.authorize(public_key, &principal) {
        return Err(Error::from(format!(
            "client key is not authorized for principal {principal}"
        )));
    }

    let mut second = vec![0_u8; MAX_NOISE_MESSAGE_BYTES];
    let second_len = handshake
        .write_message(SERVER_ACK, &mut second)
        .map_err(noise_error("write server authentication message"))?;
    write_frame(&mut stream, &second[..second_len])?;
    let transport = finish_handshake(handshake)?;
    restore_timeouts(&stream, previous)?;
    Ok(AuthenticatedSession {
        stream: SecureStream::new(stream, transport),
        peer: PeerIdentity {
            principal,
            public_key,
        },
    })
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

type SocketTimeouts = (Option<Duration>, Option<Duration>);

fn install_handshake_timeout(stream: &TcpStream, timeout: Duration) -> Result<SocketTimeouts> {
    if timeout.is_zero() {
        return Err(Error::from(
            "authentication handshake timeout must be nonzero",
        ));
    }
    let previous = (
        stream
            .read_timeout()
            .map_err(|error| Error::from(format!("read TCP timeout: {error}")))?,
        stream
            .write_timeout()
            .map_err(|error| Error::from(format!("read TCP timeout: {error}")))?,
    );
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(|error| Error::from(format!("set authentication handshake timeout: {error}")))?;
    Ok(previous)
}

fn restore_timeouts(stream: &TcpStream, timeouts: SocketTimeouts) -> Result<()> {
    stream
        .set_read_timeout(timeouts.0)
        .and_then(|()| stream.set_write_timeout(timeouts.1))
        .map_err(|error| Error::from(format!("restore TCP timeout: {error}")))
}

fn configure_transport_socket(stream: &TcpStream) -> Result<()> {
    stream
        .set_nodelay(true)
        .map_err(|error| Error::from(format!("set TCP_NODELAY: {error}")))
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
}
