use session::feed::{FeedEventBus, FeedWake, FeedWorkerConfig};
use session::NamespaceCache;
use std::time::Duration;

#[test]
fn feed_configuration_does_not_expose_consumer_lifecycle_controls() {
    let config = FeedWorkerConfig {
        path: "/changes/recent".to_string(),
        stream_path: "/changes/stream".to_string(),
        cursor_template: Some("/changes/after/{event_id}".to_string()),
        cache: Some(NamespaceCache::new()),
        event_bus: Some(FeedEventBus::new(16)),
        wake: Some(FeedWake::new()),
        reconnect_delay: Duration::from_secs(1),
        lookup_timeout: Duration::from_secs(2),
        read_timeout: Duration::from_secs(3),
        control_timeout: Duration::from_secs(4),
        backpressure_limit: 16,
    };

    assert_eq!(config.path, "/changes/recent");
    assert_eq!(config.backpressure_limit, 16);
}
