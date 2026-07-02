use crate::{transport::ClientStream, Error, Result};
use r9p::{message::Tag, multiplex::MultiplexedClient};
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Clone, Default)]
pub struct RequestTracker {
    inner: Arc<Mutex<RequestTrackerState>>,
}

#[derive(Default)]
struct RequestTrackerState {
    active: BTreeMap<u64, Vec<TrackedRequest>>,
    interrupted: BTreeSet<u64>,
}

#[derive(Clone)]
struct TrackedRequest {
    tag: Tag,
    client: MultiplexedClient<ClientStream>,
}

pub(crate) struct ActiveRequestGuard {
    tracker: RequestTracker,
    unique: Option<u64>,
    tag: Tag,
}

impl RequestTracker {
    pub(crate) fn track_current(
        &self,
        tag: Tag,
        client: MultiplexedClient<ClientStream>,
    ) -> ActiveRequestGuard {
        let Some(unique) = current_fuse_unique() else {
            return ActiveRequestGuard {
                tracker: self.clone(),
                unique: None,
                tag,
            };
        };
        let should_flush = {
            let mut state = self.inner.lock().ok();
            if let Some(state) = state.as_mut() {
                state
                    .active
                    .entry(unique)
                    .or_default()
                    .push(TrackedRequest {
                        tag,
                        client: client.clone(),
                    });
                state.interrupted.contains(&unique)
            } else {
                false
            }
        };
        if should_flush {
            let _ = client.flush_tag_timeout(tag, Duration::from_millis(250));
        }
        ActiveRequestGuard {
            tracker: self.clone(),
            unique: Some(unique),
            tag,
        }
    }

    pub(crate) fn interrupt(&self, unique: u64, timeout: Duration) -> Result<usize> {
        let requests = {
            let mut state = self
                .inner
                .lock()
                .map_err(|_| Error::new(libc::EIO, "session request tracker lock poisoned"))?;
            state.interrupted.insert(unique);
            state.active.get(&unique).cloned().unwrap_or_default()
        };
        for request in &requests {
            let _ = request.client.flush_tag_timeout(request.tag, timeout);
        }
        Ok(requests.len())
    }

    fn finish(&self, unique: Option<u64>, tag: Tag) {
        let Some(unique) = unique else {
            return;
        };
        let Ok(mut state) = self.inner.lock() else {
            return;
        };
        let remove_unique = if let Some(requests) = state.active.get_mut(&unique) {
            requests.retain(|request| request.tag != tag);
            requests.is_empty()
        } else {
            true
        };
        if remove_unique {
            state.active.remove(&unique);
            state.interrupted.remove(&unique);
        }
    }
}

impl Drop for ActiveRequestGuard {
    fn drop(&mut self) {
        self.tracker.finish(self.unique, self.tag);
    }
}

thread_local! {
    static CURRENT_FUSE_UNIQUE: Cell<Option<u64>> = const { Cell::new(None) };
}

pub fn with_fuse_unique<T>(unique: u64, run: impl FnOnce() -> T) -> T {
    struct Guard {
        previous: Option<u64>,
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            CURRENT_FUSE_UNIQUE.with(|cell| cell.set(self.previous));
        }
    }

    CURRENT_FUSE_UNIQUE.with(|cell| {
        let previous = cell.replace(Some(unique));
        let _guard = Guard { previous };
        run()
    })
}

fn current_fuse_unique() -> Option<u64> {
    CURRENT_FUSE_UNIQUE.with(Cell::get)
}
