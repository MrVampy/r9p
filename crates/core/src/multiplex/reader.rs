use crate::{
    client::{Client as ProtocolClient, ClientResponse, Completion, Op},
    client_support::{io_error, protocol_error},
    codec,
    error::{Error, Result},
    message::{RMessage, TMessage, Tag},
};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    sync::{mpsc::Sender, Arc, Mutex},
};

use super::util::{fail_all, lock, response_tag};

pub(super) type ReplyResult = std::result::Result<ClientResponse, Error>;

#[derive(Default)]
pub(super) struct ResponseState {
    waiters: BTreeMap<Tag, Sender<ReplyResult>>,
    terminal_error: Option<Error>,
}

impl ResponseState {
    pub(super) fn register(&mut self, tag: Tag, sender: Sender<ReplyResult>) -> Result<()> {
        if let Some(error) = &self.terminal_error {
            return Err(error.clone());
        }
        if self.waiters.insert(tag, sender).is_some() {
            return Err(Error::from(format!("duplicate waiter for tag {tag}")));
        }
        Ok(())
    }

    pub(super) fn remove(&mut self, tag: Tag) -> Option<Sender<ReplyResult>> {
        self.waiters.remove(&tag)
    }

    pub(super) fn terminate(&mut self, error: Error) -> Vec<Sender<ReplyResult>> {
        if self.terminal_error.is_none() {
            self.terminal_error = Some(error);
        }
        std::mem::take(&mut self.waiters).into_values().collect()
    }
}

pub(super) fn reader_loop<S: super::MultiplexTransport>(
    mut reader: S,
    protocol: Arc<Mutex<ProtocolClient>>,
    responses: Arc<Mutex<ResponseState>>,
) {
    loop {
        let max_frame_size = match lock(&protocol, "lock 9P protocol client") {
            Ok(protocol) => protocol.msize(),
            Err(error) => {
                fail_all(&responses, error);
                break;
            }
        };
        let response = match read_response(&mut reader, max_frame_size) {
            Ok(response) => response,
            Err(error) => {
                fail_all(&responses, error);
                break;
            }
        };
        let response = match lock(&protocol, "lock 9P protocol client")
            .and_then(|mut protocol| protocol.receive(response).map_err(protocol_error))
        {
            Ok(response) => response,
            Err(error) if error.message() == b"9P client state: unknown response tag" => continue,
            Err(error) => {
                fail_all(&responses, error);
                break;
            }
        };
        let tag = response_tag(&response);
        let sender = match lock(&responses, "lock 9P response state") {
            Ok(mut responses) => responses.remove(tag),
            Err(error) => {
                fail_all(&responses, error);
                break;
            }
        };
        if let Some(sender) = sender {
            let _ = sender.send(Ok(response));
        }
    }
    let _ = reader.shutdown_transport();
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
    let tag = message.tag();
    let frame = match codec::encode_tmessage_checked(&message, protocol.msize()) {
        Ok(frame) => frame,
        Err(error) => {
            protocol.abandon(tag);
            return Err(Error::from(format!("encode 9P frame: {error}")));
        }
    };
    if let Err(error) = writer.write_all(&frame) {
        protocol.abandon(tag);
        return Err(io_error("write 9P frame", error));
    }
    let response = read_response(reader, protocol.msize())?;
    protocol.receive(response).map_err(protocol_error)
}

pub(super) fn read_response(reader: &mut impl Read, max_frame_size: u32) -> Result<RMessage> {
    codec::read_rmessage_checked(reader, max_frame_size)?
        .ok_or_else(|| Error::from("9P transport closed before response"))
}

#[cfg(test)]
pub(super) fn write_response(writer: &mut impl Write, message: &RMessage) -> Result<()> {
    let frame = codec::encode_rmessage(message)
        .map_err(|error| Error::from(format!("encode 9P frame: {error}")))?;
    writer
        .write_all(&frame)
        .map_err(|error| io_error("write 9P frame", error))
}
