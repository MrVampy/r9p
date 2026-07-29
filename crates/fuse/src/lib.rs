mod diagnostics;
mod error;
mod fuse;
mod node;

pub use error::Error;
pub use fuse::{
    default_congestion_threshold, Config, DEFAULT_ATTR_TIMEOUT, DEFAULT_ENTRY_TIMEOUT,
    DEFAULT_MAX_BACKGROUND, DEFAULT_MAX_WORKERS, DEFAULT_NEGATIVE_TIMEOUT,
};

pub fn mount(config: Config) -> Result<(), Error> {
    fuse::R9pFuse::mount(config)
}

pub fn mount_with_session(
    config: Config,
    client: session::ClientSession,
    feed_events: Option<session::feed::FeedEventReceiver>,
) -> Result<(), Error> {
    fuse::R9pFuse::mount_with_session(config, client, feed_events)
}
