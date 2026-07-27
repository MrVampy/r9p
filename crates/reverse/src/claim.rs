use std::io::{self, Read, Write};

use r9p_auth::SecureStream;

const SESSION_CLAIM: &[u8] = b"r9p-reverse-session-claim.v1";

pub(crate) fn send_session_claim(stream: &mut SecureStream) -> io::Result<()> {
    stream.write_all(SESSION_CLAIM)?;
    stream.flush()
}

pub(crate) fn receive_session_claim(stream: &mut SecureStream) -> io::Result<()> {
    let mut claim = [0_u8; SESSION_CLAIM.len()];
    stream.read_exact(&mut claim)?;
    if claim == SESSION_CLAIM {
        Ok(())
    } else {
        let _ = stream.shutdown();
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "reverse stream has an invalid session claim",
        ))
    }
}
