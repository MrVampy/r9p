use crate::{
    client::ClientResponse,
    error::{Error, Result},
    message::Tag,
};
use std::sync::{Mutex, MutexGuard};

use super::reader::ResponseState;

pub(super) fn fail_all(responses: &Mutex<ResponseState>, error: Error) {
    let pending = match responses.lock() {
        Ok(mut responses) => responses.terminate(error.clone()),
        Err(poisoned) => poisoned.into_inner().terminate(error.clone()),
    };
    for sender in pending {
        let _ = sender.send(Err(error.clone()));
    }
}

pub(super) fn response_tag(response: &ClientResponse) -> Tag {
    match response {
        ClientResponse::Completion { tag, .. } | ClientResponse::Error { tag, .. } => *tag,
    }
}

pub(super) fn lock<'a, T>(mutex: &'a Mutex<T>, context: &'static str) -> Result<MutexGuard<'a, T>> {
    mutex.lock().map_err(|_| Error::from(context))
}
