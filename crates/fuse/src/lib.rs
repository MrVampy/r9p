mod error;
mod fuse;
mod node;
mod p9;

pub use error::Error;
pub use fuse::Config;

pub fn mount(config: Config) -> Result<(), Error> {
    fuse::R9pFuse::mount(config)
}
