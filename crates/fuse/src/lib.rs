mod diagnostics;
mod error;
mod fuse;
mod node;

pub use error::Error;
pub use fuse::{
    default_congestion_threshold, Config, MountHandle, DEFAULT_ATTR_TIMEOUT, DEFAULT_ENTRY_TIMEOUT,
    DEFAULT_MAX_BACKGROUND, DEFAULT_MAX_WORKERS, DEFAULT_NEGATIVE_TIMEOUT,
    MAX_PERSISTENT_READ_CACHE_BYTES, PERSISTENT_READ_CACHE_CHUNK_BYTES,
};

pub fn start(config: Config) -> Result<MountHandle, Error> {
    MountHandle::start(config)
}

pub fn start_all(configs: Vec<Config>) -> Result<Vec<MountHandle>, Error> {
    MountHandle::start_all(configs)
}

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
