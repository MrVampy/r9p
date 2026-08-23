use super::NamespaceChange;
use crate::{Error, Result as R9pResult};
use std::collections::VecDeque;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{RecvError, RecvTimeoutError},
    Arc, Condvar, Mutex, Weak,
};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub enum FeedEvent {
    Change {
        change: NamespaceChange,
        source: &'static str,
        cursor_complete: bool,
    },
    CoarseInvalidation {
        reason: String,
    },
}

#[derive(Clone, Debug)]
pub struct FeedEventBus {
    inner: Arc<FeedEventBusInner>,
    capacity: usize,
}

pub struct FeedEventReceiver {
    subscriber: Arc<FeedSubscriber>,
}

#[derive(Clone, Debug)]
pub struct FeedReceiverWake {
    subscriber: Weak<FeedSubscriber>,
}

#[derive(Debug)]
struct FeedEventBusInner {
    subscribers: Mutex<Vec<Weak<FeedSubscriber>>>,
}

#[derive(Debug)]
struct FeedSubscriber {
    state: Mutex<FeedSubscriberState>,
    changed: Condvar,
    capacity: usize,
}

#[derive(Debug, Default)]
struct FeedSubscriberState {
    queue: VecDeque<FeedEvent>,
    closed: bool,
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
            inner: Arc::new(FeedEventBusInner {
                subscribers: Mutex::new(Vec::new()),
            }),
            capacity: capacity.max(1),
        }
    }

    pub fn subscribe(&self) -> FeedEventReceiver {
        let subscriber = Arc::new(FeedSubscriber {
            state: Mutex::new(FeedSubscriberState::default()),
            changed: Condvar::new(),
            capacity: self.capacity,
        });
        if let Ok(mut subscribers) = self.inner.subscribers.lock() {
            subscribers.push(Arc::downgrade(&subscriber));
        } else {
            subscriber.close();
        }
        FeedEventReceiver { subscriber }
    }

    pub fn publish(&self, event: FeedEvent) -> bool {
        let Ok(mut subscribers) = self.inner.subscribers.lock() else {
            return false;
        };
        let mut all_delivered = true;
        subscribers.retain(|subscriber| {
            let Some(subscriber) = subscriber.upgrade() else {
                return false;
            };
            if subscriber.publish(event.clone()) {
                true
            } else {
                all_delivered = false;
                subscriber.close();
                false
            }
        });
        all_delivered
    }
}

impl FeedEventReceiver {
    pub fn recv(&self) -> std::result::Result<FeedEvent, RecvError> {
        self.subscriber.recv()
    }

    pub fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> std::result::Result<FeedEvent, RecvTimeoutError> {
        self.subscriber.recv_timeout(timeout)
    }

    pub fn wake_handle(&self) -> FeedReceiverWake {
        FeedReceiverWake {
            subscriber: Arc::downgrade(&self.subscriber),
        }
    }

    pub fn recv_until_stopped(
        &self,
        stop: &AtomicBool,
    ) -> std::result::Result<Option<FeedEvent>, std::sync::mpsc::RecvError> {
        self.subscriber.recv_until_stopped(stop)
    }
}

impl FeedReceiverWake {
    pub fn notify(&self) {
        if let Some(subscriber) = self.subscriber.upgrade() {
            subscriber.changed.notify_all();
        }
    }
}

impl FeedSubscriber {
    fn publish(&self, event: FeedEvent) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if state.closed || state.queue.len() >= self.capacity {
            return false;
        }
        state.queue.push_back(event);
        self.changed.notify_one();
        true
    }

    fn recv(&self) -> std::result::Result<FeedEvent, RecvError> {
        let mut state = self.state.lock().map_err(|_| RecvError)?;
        loop {
            if let Some(event) = state.queue.pop_front() {
                return Ok(event);
            }
            if state.closed {
                return Err(RecvError);
            }
            state = self.changed.wait(state).map_err(|_| RecvError)?;
        }
    }

    fn recv_timeout(&self, timeout: Duration) -> std::result::Result<FeedEvent, RecvTimeoutError> {
        let deadline = Instant::now() + timeout;
        let mut state = self
            .state
            .lock()
            .map_err(|_| RecvTimeoutError::Disconnected)?;
        loop {
            if let Some(event) = state.queue.pop_front() {
                return Ok(event);
            }
            if state.closed {
                return Err(RecvTimeoutError::Disconnected);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(RecvTimeoutError::Timeout);
            }
            let (next, timed_out) = self
                .changed
                .wait_timeout(state, remaining)
                .map_err(|_| RecvTimeoutError::Disconnected)?;
            state = next;
            if timed_out.timed_out() && state.queue.is_empty() {
                return Err(RecvTimeoutError::Timeout);
            }
        }
    }

    fn recv_until_stopped(
        &self,
        stop: &AtomicBool,
    ) -> std::result::Result<Option<FeedEvent>, RecvError> {
        let mut state = self.state.lock().map_err(|_| RecvError)?;
        loop {
            if stop.load(Ordering::SeqCst) {
                return Ok(None);
            }
            if let Some(event) = state.queue.pop_front() {
                return Ok(Some(event));
            }
            if state.closed {
                return Err(RecvError);
            }
            state = self.changed.wait(state).map_err(|_| RecvError)?;
        }
    }

    fn close(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.closed = true;
            self.changed.notify_all();
        }
    }
}

impl Drop for FeedEventBusInner {
    fn drop(&mut self) {
        if let Ok(subscribers) = self.subscribers.get_mut() {
            for subscriber in subscribers.iter().filter_map(Weak::upgrade) {
                subscriber.close();
            }
        }
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

    pub(crate) fn wait_until_closed_or_timeout(&self, timeout: Duration) -> R9pResult<()> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| Error::new(libc::EIO, "namespace feed wake lock poisoned"))?;
        let (state, _) = self
            .inner
            .changed
            .wait_timeout_while(state, timeout, |state| !state.closed)
            .map_err(|_| Error::new(libc::EIO, "namespace feed wake lock poisoned"))?;
        if state.closed {
            Err(Error::new(libc::ESHUTDOWN, "namespace feed wake is closed"))
        } else {
            Ok(())
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
    use super::{FeedEvent, FeedEventBus, FeedWake};
    use std::{
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        thread,
        time::{Duration, Instant},
    };

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

    #[test]
    fn retry_deadline_is_not_bypassed_by_change_notifications() {
        let wake = FeedWake::new();
        let waiter = {
            let wake = wake.clone();
            thread::spawn(move || {
                let started = Instant::now();
                wake.wait_until_closed_or_timeout(Duration::from_millis(50))
                    .expect("deadline");
                started.elapsed()
            })
        };

        for _ in 0..10_000 {
            wake.notify();
        }

        assert!(waiter.join().expect("waiter") >= Duration::from_millis(40));
    }

    #[test]
    fn close_interrupts_a_retry_deadline() {
        let wake = FeedWake::new();
        let waiter = {
            let wake = wake.clone();
            thread::spawn(move || wake.wait_until_closed_or_timeout(Duration::from_secs(60)))
        };

        wake.close();

        assert_eq!(
            waiter.join().expect("waiter").expect_err("closed").errno,
            libc::ESHUTDOWN
        );
    }

    #[test]
    fn receiver_wake_releases_an_event_wait_without_polling() {
        let bus = FeedEventBus::default();
        let receiver = bus.subscribe();
        let wake = receiver.wake_handle();
        let stopped = Arc::new(AtomicBool::new(false));
        let waiter_stop = Arc::clone(&stopped);
        let waiter = thread::spawn(move || {
            receiver
                .recv_until_stopped(&waiter_stop)
                .expect("receiver remains connected")
        });

        stopped.store(true, Ordering::SeqCst);
        wake.notify();

        assert!(waiter.join().expect("waiter").is_none());
    }

    #[test]
    fn subscriber_backpressure_disconnects_after_retained_events_are_drained() {
        let bus = FeedEventBus::new(1);
        let receiver = bus.subscribe();
        assert!(bus.publish(FeedEvent::CoarseInvalidation {
            reason: "first".to_string(),
        }));
        assert!(!bus.publish(FeedEvent::CoarseInvalidation {
            reason: "overflow".to_string(),
        }));
        assert!(matches!(
            receiver.recv().expect("retained event"),
            FeedEvent::CoarseInvalidation { reason } if reason == "first"
        ));
        assert!(receiver.recv().is_err());
    }
}
