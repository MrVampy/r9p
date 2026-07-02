use super::{tree::ControlTree, ControlConfig};
use crate::{feed::FeedState, Client, Error, NamespaceCache, Result};
use r9p::{
    codec,
    message::TMessage,
    server::{Server, ServerConfig},
};
use std::io::{Read, Write};

const CONTROL_MSIZE: u32 = 65_536;

pub(super) fn serve_control_connection<S>(
    mut stream: S,
    client: Client,
    config: ControlConfig,
    feed_state: FeedState,
    cache: NamespaceCache,
    session_epoch: String,
) -> Result<()>
where
    S: Read + Write,
{
    let mut server = Server::with_config(
        ControlTree::new(client, config, feed_state, cache, session_epoch),
        ServerConfig {
            default_msize: CONTROL_MSIZE,
            max_msize: CONTROL_MSIZE,
            ..ServerConfig::default()
        },
    );

    loop {
        let Some(message) = read_tmessage(&mut stream)? else {
            return Ok(());
        };
        let reply = server.handle(message);
        let frame =
            codec::encode_rmessage_checked(&reply, server.session().msize()).map_err(|error| {
                Error::new(
                    libc::EPROTO,
                    format!("encode control 9P reply: {}", error.display_lossy()),
                )
            })?;
        stream
            .write_all(&frame)
            .map_err(|error| Error::io("write control 9P reply", error))?;
        stream
            .flush()
            .map_err(|error| Error::io("flush control 9P reply", error))?;
    }
}

fn read_tmessage(stream: &mut impl Read) -> Result<Option<TMessage>> {
    let mut prefix = [0_u8; 4];
    match stream.read_exact(&mut prefix) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(Error::io("read control 9P frame size", error)),
    }
    let size = u32::from_le_bytes(prefix);
    if size < codec::FRAME_HEADER_SIZE {
        return Err(Error::new(
            libc::EPROTO,
            format!("short control 9P frame {size}"),
        ));
    }
    let rest_len = usize::try_from(size - 4)
        .map_err(|_| Error::new(libc::EOVERFLOW, "control 9P frame too large"))?;
    let mut frame = Vec::with_capacity(rest_len + 4);
    frame.extend(prefix);
    frame.resize(rest_len + 4, 0);
    stream
        .read_exact(&mut frame[4..])
        .map_err(|error| Error::io("read control 9P frame body", error))?;
    codec::decode_tmessage(&frame)
        .map(Some)
        .map_err(|error| Error::new(libc::EPROTO, error.display_lossy().to_string()))
}
