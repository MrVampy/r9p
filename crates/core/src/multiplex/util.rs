use crate::{
    client::{ClientResponse, Op},
    error::{Error, Result},
    fid::Fid,
    message::Tag,
};
use std::{
    io,
    sync::{Mutex, MutexGuard},
};

use super::reader::Waiters;

pub(super) fn fail_all(waiters: &Mutex<Waiters>, error: Error) {
    if let Ok(mut waiters) = waiters.lock() {
        let pending = std::mem::take(&mut *waiters);
        for sender in pending.into_values() {
            let _ = sender.send(Err(error.clone()));
        }
    }
}

pub(super) fn response_tag(response: &ClientResponse) -> Tag {
    match response {
        ClientResponse::Completion { tag, .. } | ClientResponse::Error { tag, .. } => *tag,
    }
}

pub(super) fn op_fid(op: &Op) -> Result<Fid> {
    op.fid
        .ok_or_else(|| Error::from("9P operation did not allocate a fid"))
}

pub(super) fn protocol_error(error: Error) -> Error {
    Error::from(format!("9P client state: {error}"))
}

pub(super) fn io_error(context: impl AsRef<str>, error: io::Error) -> Error {
    if matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ) {
        return Error::from(format!(
            "{}: 9P transport timeout or would-block: {error}",
            context.as_ref()
        ));
    }
    Error::from(format!("{}: {error}", context.as_ref()))
}

pub(super) fn unexpected(expected: &str, got: impl std::fmt::Debug) -> Error {
    Error::from(format!("expected {expected}, got {got:?}"))
}

pub(super) fn lock<'a, T>(mutex: &'a Mutex<T>, context: &'static str) -> Result<MutexGuard<'a, T>> {
    mutex.lock().map_err(|_| Error::from(context))
}
