use crate::{
    client::ClientResponse,
    error::{Error, Result},
    message::Tag,
};
use std::sync::{Mutex, MutexGuard};

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

pub(super) fn lock<'a, T>(mutex: &'a Mutex<T>, context: &'static str) -> Result<MutexGuard<'a, T>> {
    mutex.lock().map_err(|_| Error::from(context))
}
