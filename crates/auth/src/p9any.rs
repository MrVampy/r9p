use crate::AUTH_PROTOCOL_XX;
use r9p::error::{Error, Result};
use std::io::{Read, Write};

const P9ANY_VERSION: &str = "v.2";
const MAX_NEGOTIATION_BYTES: usize = 2048;

pub(crate) fn negotiate_server<S: Read + Write>(stream: &mut S, domain: &str) -> Result<()> {
    write_nul_string(
        stream,
        &format!("{P9ANY_VERSION} {AUTH_PROTOCOL_XX}@{domain}"),
    )?;
    let selection = read_nul_string(stream, "p9any protocol selection")?;
    if selection != format!("{AUTH_PROTOCOL_XX} {domain}") {
        return Err(Error::from("p9any client selected an unsupported protocol"));
    }
    write_nul_string(stream, "OK")?;
    Ok(())
}

pub(crate) fn negotiate_client<S: Read + Write>(stream: &mut S, domain: &str) -> Result<()> {
    let offer = read_nul_string(stream, "p9any protocol offer")?;
    let mut fields = offer.split_ascii_whitespace();
    if fields.next() != Some(P9ANY_VERSION) {
        return Err(Error::from("p9any server did not offer version 2"));
    }
    if !fields.any(|offered| offered == format!("{AUTH_PROTOCOL_XX}@{domain}")) {
        return Err(Error::from(format!(
            "p9any server offered nothing this client can speak for domain {domain}"
        )));
    }
    write_nul_string(stream, &format!("{AUTH_PROTOCOL_XX} {domain}"))?;
    let response = read_nul_string(stream, "p9any selection response")?;
    if response != "OK" {
        return Err(Error::from("p9any server rejected protocol selection"));
    }
    Ok(())
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

    #[test]
    fn both_sides_settle_on_xx() -> Result<()> {
        let listener =
            TcpListener::bind("127.0.0.1:0").map_err(|error| Error::from(error.to_string()))?;
        let address = listener
            .local_addr()
            .map_err(|error| Error::from(error.to_string()))?;
        let server = thread::spawn(move || -> Result<()> {
            let (mut stream, _) = listener
                .accept()
                .map_err(|error| Error::from(error.to_string()))?;
            negotiate_server(&mut stream, "vault")
        });
        let mut stream = std::net::TcpStream::connect(address)
            .map_err(|error| Error::from(error.to_string()))?;
        negotiate_client(&mut stream, "vault")?;
        server
            .join()
            .map_err(|_| Error::from("p9any server panicked"))??;
        Ok(())
    }

    #[test]
    fn p9any_version_two_negotiates_the_exact_domain() -> Result<()> {
        let listener =
            TcpListener::bind("127.0.0.1:0").map_err(|error| Error::from(error.to_string()))?;
        let address = listener
            .local_addr()
            .map_err(|error| Error::from(error.to_string()))?;
        let server = thread::spawn(move || -> Result<()> {
            let (mut stream, _) = listener
                .accept()
                .map_err(|error| Error::from(error.to_string()))?;
            negotiate_server(&mut stream, "vault")
        });
        let mut stream = std::net::TcpStream::connect(address)
            .map_err(|error| Error::from(error.to_string()))?;
        assert!(negotiate_client(&mut stream, "other-domain").is_err());
        // The client never sends a selection, so the server is still blocked on
        // the read. Close first or the join deadlocks.
        drop(stream);
        assert!(server.join().map_err(|_| Error::from("panic"))?.is_err());
        Ok(())
    }
}
