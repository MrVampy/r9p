mod authority;
mod cache;
mod client;
mod client_session;
mod connection_config;
pub mod control;
mod epoch;
mod error;
pub mod feed;
pub mod materialization;
mod opened_fid;
#[cfg(unix)]
mod projection;
mod request;
mod resumable_fid;
mod transport;

pub use cache::{
    decode_dir_entries, is_dir, is_symlink, read_open_directory_entries, same_qid,
    validate_directory_entries, DirCache, DirEntry, Freshness, NamespaceCache, NamespaceCacheStats,
    StaleReason,
};
pub use client::Client;
pub use client_session::{ClientSession, PreparedClientSession};
pub use connection_config::{
    ClientCredential, ConnectionAuthentication, ConnectionConfig, ConnectionSet, ResponderName,
    SessionAuthentication, MAX_CONNECTION_CANDIDATES,
};
pub use epoch::SessionEpoch;
pub use error::{errno_for_9p_error, p9_error, Error, Result, WriteThenReadError};
pub use opened_fid::{ConcurrentReadFid, OpenedFid};
#[cfg(unix)]
pub use projection::{NamespaceProjection, NamespaceProjectionConfig, NamespaceProjectionStatus};
pub use r9p::{ORDWR, OREAD, OTRUNC, OWRITE};
pub use request::{with_fuse_unique, RequestTracker};
pub use resumable_fid::ResumableFid;

/// Parses a canonical namespace path into 9P path elements.
///
/// Both absolute and root-relative spellings are accepted. Empty elements,
/// trailing slashes, NUL bytes, and `.` or `..` elements are rejected.
pub fn parse_namespace_path(path: &[u8]) -> Result<Vec<Vec<u8>>> {
    client::parse_namespace_path(path)
}
