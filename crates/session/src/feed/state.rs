use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub struct FeedState {
    inner: Arc<Mutex<FeedSnapshot>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedSnapshot {
    pub state: &'static str,
    pub source: Option<&'static str>,
    pub last_event_id: Option<String>,
    pub last_generation: Option<u64>,
    pub last_error: Option<String>,
    pub fresh_instance: bool,
}

impl FeedState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(FeedSnapshot {
                state: "disabled",
                source: None,
                last_event_id: None,
                last_generation: None,
                last_error: None,
                fresh_instance: false,
            })),
        }
    }

    pub(crate) fn with_cursor(event_id: String, generation: u64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(FeedSnapshot {
                state: "disabled",
                source: None,
                last_event_id: Some(event_id),
                last_generation: Some(generation),
                last_error: None,
                fresh_instance: false,
            })),
        }
    }

    pub fn snapshot(&self) -> FeedSnapshot {
        self.inner
            .lock()
            .map(|snapshot| snapshot.clone())
            .unwrap_or_else(|_| FeedSnapshot {
                state: "degraded",
                source: None,
                last_event_id: None,
                last_generation: None,
                last_error: Some("feed state lock poisoned".to_string()),
                fresh_instance: true,
            })
    }

    pub fn set_disabled(&self) {
        self.replace(FeedSnapshot {
            state: "disabled",
            source: None,
            last_event_id: None,
            last_generation: None,
            last_error: None,
            fresh_instance: false,
        });
    }

    pub fn set_connecting(&self) {
        self.update(|snapshot| {
            snapshot.state = "connecting";
            snapshot.source = None;
            snapshot.last_error = None;
        });
    }

    pub fn set_connected(
        &self,
        source: &'static str,
        event_id: Option<String>,
        generation: Option<u64>,
    ) {
        self.update(|snapshot| {
            snapshot.state = "connected";
            snapshot.source = Some(source);
            if event_id.is_some() {
                snapshot.last_event_id = event_id;
            }
            if generation.is_some() {
                snapshot.last_generation = generation;
            }
            snapshot.last_error = None;
        });
    }

    pub fn set_degraded(&self, message: impl Into<String>) {
        self.update(|snapshot| {
            snapshot.state = "degraded";
            snapshot.last_error = Some(message.into());
        });
    }

    pub fn mark_fresh_instance(&self) {
        self.update(|snapshot| {
            snapshot.fresh_instance = true;
        });
    }

    fn replace(&self, next: FeedSnapshot) {
        if let Ok(mut snapshot) = self.inner.lock() {
            *snapshot = next;
        }
    }

    fn update(&self, body: impl FnOnce(&mut FeedSnapshot)) {
        if let Ok(mut snapshot) = self.inner.lock() {
            body(&mut snapshot);
        }
    }
}

impl Default for FeedState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::FeedState;

    #[test]
    fn feed_state_tracks_connected_cursor() {
        let state = FeedState::new();

        state.set_connecting();
        state.set_connected("stream", Some("event-7".to_string()), Some(42));

        let snapshot = state.snapshot();
        assert_eq!(snapshot.state, "connected");
        assert_eq!(snapshot.source, Some("stream"));
        assert_eq!(snapshot.last_event_id.as_deref(), Some("event-7"));
        assert_eq!(snapshot.last_generation, Some(42));
        assert_eq!(snapshot.last_error, None);
    }

    #[test]
    fn retained_cursor_survives_the_connecting_transition() {
        let state = FeedState::with_cursor("g4-s9".to_string(), 4);
        state.set_connecting();

        let snapshot = state.snapshot();
        assert_eq!(snapshot.state, "connecting");
        assert_eq!(snapshot.last_event_id.as_deref(), Some("g4-s9"));
        assert_eq!(snapshot.last_generation, Some(4));
    }
}
