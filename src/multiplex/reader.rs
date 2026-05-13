use crate::{
    client::{Client as ProtocolClient, ClientResponse, Completion, Op},
    codec,
    error::{Error, Result},
    message::{RMessage, TMessage, Tag},
};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    sync::{mpsc::Sender, Arc, Mutex},
};

use super::util::{fail_all, io_error, lock, protocol_error, response_tag};

pub(super) type ReplyResult = std::result::Result<ClientResponse, Error>;
pub(super) type Waiters = BTreeMap<Tag, Sender<ReplyResult>>;

pub(super) fn reader_loop<S: super::MultiplexTransport>(
    mut reader: S,
    protocol: Arc<Mutex<ProtocolClient>>,
    waiters: Arc<Mutex<Waiters>>,
) {
    loop {
        let response = match read_response(&mut reader) {
            Ok(response) => response,
            Err(error) => {
                fail_all(&waiters, error);
                return;
            }
        };
        let response = match lock(&protocol, "lock 9P protocol client")
            .and_then(|mut protocol| protocol.receive(response).map_err(protocol_error))
        {
            Ok(response) => response,
            Err(error) if error.message() == b"9P client state: unknown response tag" => continue,
            Err(error) => {
                fail_all(&waiters, error);
                return;
            }
        };
        let tag = response_tag(&response);
        let sender = match lock(&waiters, "lock 9P waiter table") {
            Ok(mut waiters) => waiters.remove(&tag),
            Err(error) => {
                fail_all(&waiters, error);
                return;
            }
        };
        if let Some(sender) = sender {
            let _ = sender.send(Ok(response));
        }
    }
}

pub(super) fn call_op_sync<S: Read + Write>(
    writer: &mut S,
    reader: &mut S,
    protocol: &mut ProtocolClient,
    op: Op,
) -> Result<Completion> {
    let expected_tag = op.tag;
    match call_message_sync(writer, reader, protocol, op.message)? {
        ClientResponse::Completion { tag, completion } if tag == expected_tag => Ok(completion),
        ClientResponse::Error { tag, ename } if tag == expected_tag => Err(Error::new(ename)),
        other => Err(Error::from(format!(
            "response tag mismatch or unexpected response: {other:?}"
        ))),
    }
}

pub(super) fn call_message_sync<S: Read + Write>(
    writer: &mut S,
    reader: &mut S,
    protocol: &mut ProtocolClient,
    message: TMessage,
) -> Result<ClientResponse> {
    let frame = codec::encode_tmessage(&message)
        .map_err(|error| Error::from(format!("encode 9P frame: {error}")))?;
    writer
        .write_all(&frame)
        .map_err(|error| io_error("write 9P frame", error))?;
    let response = read_response(reader)?;
    protocol.receive(response).map_err(protocol_error)
}

pub(super) fn read_response(reader: &mut impl Read) -> Result<RMessage> {
    let mut prefix = [0_u8; 4];
    reader
        .read_exact(&mut prefix)
        .map_err(|error| io_error("read 9P frame size", error))?;
    let size = u32::from_le_bytes(prefix);
    if size < codec::FRAME_HEADER_SIZE {
        return Err(Error::from("short 9P frame"));
    }
    let rest_len = usize::try_from(size - 4).map_err(|_| Error::from("oversized 9P frame"))?;
    let mut frame = Vec::with_capacity(usize::try_from(size).unwrap_or(rest_len + 4));
    frame.extend(prefix);
    frame.resize(rest_len + 4, 0);
    reader
        .read_exact(&mut frame[4..])
        .map_err(|error| io_error("read 9P frame body", error))?;
    codec::decode_rmessage(&frame).map_err(|error| Error::from(format!("decode 9P frame: {error}")))
}

#[cfg(test)]
pub(super) fn write_response(writer: &mut impl Write, message: &RMessage) -> Result<()> {
    let frame = codec::encode_rmessage(message)
        .map_err(|error| Error::from(format!("encode 9P frame: {error}")))?;
    writer
        .write_all(&frame)
        .map_err(|error| io_error("write 9P frame", error))
}
