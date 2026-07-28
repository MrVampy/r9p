use super::NamespaceChange;
use crate::{Error, Result as R9pResult};
use std::sync::{
    mpsc::{sync_channel, Receiver, SyncSender, TrySendError},
    Arc, Condvar, Mutex,
};

#[derive(Clone, Debug)]
pub enum FeedEvent {
    Change {
        change: NamespaceChange,
        source: &'static str,
    },
    CoarseInvalidation {
        reason: String,
    },
}

#[derive(Clone, Debug)]
pub struct FeedEventBus {
    inner: Arc<Mutex<Vec<SyncSender<FeedEvent>>>>,
    capacity: usize,
}

pub struct FeedEventReceiver {
    receiver: Receiver<FeedEvent>,
}

#[derive(Clone, Debug)]
pub struct FeedWake {
    inner: Arc<FeedWakeInner>,
}

#[derive(Debug)]
struct FeedWakeInner {
    state: Mutex<FeedWakeState>,
    changed: Condvar,
}

#[derive(Debug, Default)]
struct FeedWakeState {
    generation: u64,
    closed: bool,
}

impl FeedEventBus {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Vec::new())),
            capacity: capacity.max(1),
        }
    }

    pub fn subscribe(&self) -> FeedEventReceiver {
        let (sender, receiver) = sync_channel(self.capacity);
        if let Ok(mut senders) = self.inner.lock() {
            senders.push(sender);
        }
        FeedEventReceiver { receiver }
    }

    pub fn publish(&self, event: FeedEvent) -> bool {
        let Ok(mut senders) = self.inner.lock() else {
            return false;
        };
        let mut all_delivered = true;
        senders.retain(|sender| match sender.try_send(event.clone()) {
            Ok(()) => true,
            Err(TrySendError::Disconnected(_)) => false,
            Err(TrySendError::Full(_)) => {
                all_delivered = false;
                false
            }
        });
        all_delivered
    }
}

impl FeedEventReceiver {
    pub fn recv(&self) -> std::result::Result<FeedEvent, std::sync::mpsc::RecvError> {
        self.receiver.recv()
    }

    pub fn recv_timeout(
        &self,
        timeout: std::time::Duration,
    ) -> std::result::Result<FeedEvent, std::sync::mpsc::RecvTimeoutError> {
        self.receiver.recv_timeout(timeout)
    }
}

impl Default for FeedEventBus {
    fn default() -> Self {
        Self::new(4096)
    }
}

impl FeedWake {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(FeedWakeInner {
                state: Mutex::new(FeedWakeState::default()),
                changed: Condvar::new(),
            }),
        }
    }

    pub fn generation(&self) -> R9pResult<u64> {
        self.inner
            .state
            .lock()
            .map(|state| state.generation)
            .map_err(|_| Error::new(libc::EIO, "namespace feed wake lock poisoned"))
    }

    pub fn wait_after(&self, generation: u64) -> R9pResult<u64> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| Error::new(libc::EIO, "namespace feed wake lock poisoned"))?;
        loop {
            if state.generation != generation {
                return Ok(state.generation);
            }
            if state.closed {
                return Err(Error::new(libc::ESHUTDOWN, "namespace feed wake is closed"));
            }
            state = self
                .inner
                .changed
                .wait(state)
                .map_err(|_| Error::new(libc::EIO, "namespace feed wake lock poisoned"))?;
        }
    }

    pub(crate) fn notify(&self) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.generation = state.generation.wrapping_add(1);
            self.inner.changed.notify_all();
        }
    }

    pub(crate) fn close(&self) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.closed = true;
            self.inner.changed.notify_all();
        }
    }
}

impl Default for FeedWake {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::FeedWake;
    use std::thread;

    #[test]
    fn wake_notifies_all_waiters_after_their_observed_generation() {
        let wake = FeedWake::new();
        let generation = wake.generation().expect("initial generation");
        let waiter = {
            let wake = wake.clone();
            thread::spawn(move || wake.wait_after(generation).expect("wake"))
        };

        wake.notify();

        assert_ne!(waiter.join().expect("waiter"), generation);
    }

    #[test]
    fn close_releases_a_waiter_without_a_change() {
        let wake = FeedWake::new();
        let generation = wake.generation().expect("initial generation");
        let waiter = {
            let wake = wake.clone();
            thread::spawn(move || wake.wait_after(generation))
        };

        wake.close();

        assert_eq!(
            waiter.join().expect("waiter").expect_err("closed").errno,
            libc::ESHUTDOWN
        );
    }
}
