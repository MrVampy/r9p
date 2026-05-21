mod error;
mod fuse;
mod node;
mod p9;

pub use error::Error;
pub use fuse::{default_congestion_threshold, Config, DEFAULT_MAX_BACKGROUND, DEFAULT_MAX_WORKERS};

pub fn mount(config: Config) -> Result<(), Error> {
    fuse::R9pFuse::mount(config)
}
