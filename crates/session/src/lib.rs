mod authority;
mod cache;
mod client;
mod client_session;
mod connection_config;
pub mod control;
mod epoch;
mod error;
pub mod feed;
mod opened_fid;
mod request;
mod transport;

pub use authority::AuthorityBindings;
pub use cache::{
    decode_dir_entries, is_dir, is_symlink, read_open_directory_entries, same_qid, DirCache,
    DirEntry, Freshness, NamespaceCache, NamespaceCacheStats, StaleReason,
};
pub use client::Client;
pub use client_session::ClientSession;
pub use connection_config::ConnectionConfig;
pub use epoch::SessionEpoch;
pub use error::{errno_for_9p_error, p9_error, Error, Result, WriteThenReadError};
pub use opened_fid::OpenedFid;
pub use r9p::{ORDWR, OREAD, OTRUNC, OWRITE};
pub use request::{with_fuse_unique, RequestTracker};

/// Parses a canonical namespace path into 9P path elements.
///
/// Both absolute and root-relative spellings are accepted. Empty elements,
/// trailing slashes, NUL bytes, and `.` or `..` elements are rejected.
pub fn parse_namespace_path(path: &[u8]) -> Result<Vec<Vec<u8>>> {
    client::parse_namespace_path(path)
}
