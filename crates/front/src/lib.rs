pub mod abi;
pub mod serve;

mod front;
mod model;
mod tree;

pub use front::Front;
pub(crate) use front::ReadTarget;
pub use model::{
    CreateRelayRequest, IntakeRequest, PushedDirectoryMetadata, PushedEntryMetadata,
    PushedFileMetadata, RequestContext, DEFAULT_IOUNIT, DEFAULT_LOG_CAPACITY,
};
pub use tree::FrontTree;

#[cfg(test)]
pub(crate) use model::ROOT_ID;
#[cfg(test)]
pub(crate) use r9p::error::ENOENT;
#[cfg(test)]
pub(crate) use r9p::{ORCLOSE, ORDWR, OREAD, OTRUNC, OWRITE};

#[cfg(test)]
mod tests;
