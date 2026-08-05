use crate::{AUTH_PROTOCOL, AUTH_PROTOCOL_XX};
use r9p::error::{Error, Result};
use std::io::{Read, Write};

const P9ANY_VERSION: &str = "v.2";
const MAX_NEGOTIATION_BYTES: usize = 2048;

/// Which Noise pattern the two sides agreed on. Selection happens here rather
/// than at compile time so a server can serve both while a fleet migrates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Protocol {
    NoiseIk,
    NoiseXx,
}

impl Protocol {
    const fn token(self) -> &'static str {
        match self {
            Self::NoiseIk => AUTH_PROTOCOL,
            Self::NoiseXx => AUTH_PROTOCOL_XX,
        }
    }
}

/// Offers XX first when this server can speak it, so a capable client takes the
/// stronger option and an older one still finds IK in the same offer.
pub(crate) fn negotiate_server<S: Read + Write>(
    stream: &mut S,
    domain: &str,
    offer_xx: bool,
) -> Result<Protocol> {
    let mut offered = Vec::new();
    if offer_xx {
        offered.push(Protocol::NoiseXx);
    }
    offered.push(Protocol::NoiseIk);
    let advertisement = offered
        .iter()
        .map(|protocol| format!("{}@{domain}", protocol.token()))
        .collect::<Vec<_>>()
        .join(" ");
    write_nul_string(stream, &format!("{P9ANY_VERSION} {advertisement}"))?;

    let selection = read_nul_string(stream, "p9any protocol selection")?;
    let chosen = offered
        .into_iter()
        .find(|protocol| selection == format!("{} {domain}", protocol.token()))
        .ok_or_else(|| Error::from("p9any client selected an unsupported protocol"))?;
    write_nul_string(stream, "OK")?;
    Ok(chosen)
}

/// Prefers XX when both sides can, because it is what removes the client's need
/// to pin this server's key.
pub(crate) fn negotiate_client<S: Read + Write>(
    stream: &mut S,
    domain: &str,
    can_xx: bool,
) -> Result<Protocol> {
    let offer = read_nul_string(stream, "p9any protocol offer")?;
    let mut fields = offer.split_ascii_whitespace();
    if fields.next() != Some(P9ANY_VERSION) {
        return Err(Error::from("p9any server did not offer version 2"));
    }
    let offered = fields.collect::<Vec<_>>();
    let mut preference = Vec::new();
    if can_xx {
        preference.push(Protocol::NoiseXx);
    }
    preference.push(Protocol::NoiseIk);

    let chosen = preference
        .into_iter()
        .find(|protocol| offered.contains(&format!("{}@{domain}", protocol.token()).as_str()))
        .ok_or_else(|| {
            Error::from(format!(
                "p9any server offered nothing this client can speak for domain {domain}"
            ))
        })?;
    write_nul_string(stream, &format!("{} {domain}", chosen.token()))?;
    let response = read_nul_string(stream, "p9any selection response")?;
    if response != "OK" {
        return Err(Error::from("p9any server rejected protocol selection"));
    }
    Ok(chosen)
}

fn read_nul_string<S: Read>(stream: &mut S, label: &str) -> Result<String> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0_u8; 1];
        stream
            .read_exact(&mut byte)
            .map_err(|error| Error::from(format!("read {label}: {error}")))?;
        if byte[0] == 0 {
            break;
        }
        if bytes.len() == MAX_NEGOTIATION_BYTES {
            return Err(Error::from(format!(
                "{label} exceeds {MAX_NEGOTIATION_BYTES} bytes"
            )));
        }
        bytes.push(byte[0]);
    }
    String::from_utf8(bytes).map_err(|_| Error::from(format!("{label} is not utf-8")))
}

fn write_nul_string<S: Write>(stream: &mut S, value: &str) -> Result<()> {
    if value.len() > MAX_NEGOTIATION_BYTES || value.as_bytes().contains(&0) {
        return Err(Error::from("invalid p9any negotiation string"));
    }
    stream
        .write_all(value.as_bytes())
        .and_then(|()| stream.write_all(&[0]))
        .and_then(|()| stream.flush())
        .map_err(|error| Error::from(format!("write p9any negotiation: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::TcpListener, thread};

    fn exchange(offer_xx: bool, can_xx: bool) -> Result<(Protocol, Protocol)> {
        let listener =
            TcpListener::bind("127.0.0.1:0").map_err(|error| Error::from(error.to_string()))?;
        let address = listener
            .local_addr()
            .map_err(|error| Error::from(error.to_string()))?;
        let server = thread::spawn(move || -> Result<Protocol> {
            let (mut stream, _) = listener
                .accept()
                .map_err(|error| Error::from(error.to_string()))?;
            negotiate_server(&mut stream, "vault", offer_xx)
        });
        let mut stream = std::net::TcpStream::connect(address)
            .map_err(|error| Error::from(error.to_string()))?;
        let client = negotiate_client(&mut stream, "vault", can_xx)?;
        let server = server
            .join()
            .map_err(|_| Error::from("p9any server panicked"))??;
        Ok((client, server))
    }

    #[test]
    fn both_sides_capable_settle_on_xx() -> Result<()> {
        assert_eq!(
            exchange(true, true)?,
            (Protocol::NoiseXx, Protocol::NoiseXx)
        );
        Ok(())
    }

    #[test]
    fn an_older_client_still_finds_ik_in_the_same_offer() -> Result<()> {
        assert_eq!(
            exchange(true, false)?,
            (Protocol::NoiseIk, Protocol::NoiseIk)
        );
        Ok(())
    }

    #[test]
    fn a_capable_client_falls_back_for_a_server_that_cannot() -> Result<()> {
        assert_eq!(
            exchange(false, true)?,
            (Protocol::NoiseIk, Protocol::NoiseIk)
        );
        Ok(())
    }

    #[test]
    fn p9any_version_two_negotiates_the_exact_domain() -> Result<()> {
        let listener =
            TcpListener::bind("127.0.0.1:0").map_err(|error| Error::from(error.to_string()))?;
        let address = listener
            .local_addr()
            .map_err(|error| Error::from(error.to_string()))?;
        let server = thread::spawn(move || -> Result<Protocol> {
            let (mut stream, _) = listener
                .accept()
                .map_err(|error| Error::from(error.to_string()))?;
            negotiate_server(&mut stream, "vault", true)
        });
        let mut stream = std::net::TcpStream::connect(address)
            .map_err(|error| Error::from(error.to_string()))?;
        assert!(negotiate_client(&mut stream, "other-domain", true).is_err());
        // The client never sends a selection, so the server is still blocked on
        // the read. Close first or the join deadlocks.
        drop(stream);
        assert!(server.join().map_err(|_| Error::from("panic"))?.is_err());
        Ok(())
    }
}
