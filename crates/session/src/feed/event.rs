use super::NamespaceChange;
use std::sync::{
    mpsc::{sync_channel, Receiver, SyncSender, TrySendError},
    Arc, Mutex,
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
    pub fn recv_timeout(
        &self,
        timeout: std::time::Duration,
    ) -> Result<FeedEvent, std::sync::mpsc::RecvTimeoutError> {
        self.receiver.recv_timeout(timeout)
    }
}

impl Default for FeedEventBus {
    fn default() -> Self {
        Self::new(4096)
    }
}
