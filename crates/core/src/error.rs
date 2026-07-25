use std::{borrow::Cow, fmt};

pub type Result<T> = std::result::Result<T, Error>;

pub const EPERM: &str = "permission denied";
pub const ENOENT: &str = "file does not exist";
pub const EEXIST: &str = "file exists";
pub const ENOTDIR: &str = "not a directory";
pub const EDEL: &str = "deleted window";
pub const EBADCTL: &str = "ill-formed control message";
pub const EINUSE: &str = "already in use";

pub const EBADTAG: &str = "invalid tag";
pub const EDUPTAG: &str = "duplicate tag";
pub const EBADFID: &str = "unknown fid";
pub const EFIDINUSE: &str = "fid already in use";
pub const EFIDLIMIT: &str = "fid limit exceeded";
pub const EFIDBUSY: &str = "fid busy";
pub const EFIDOPEN: &str = "fid already open";
pub const EFIDNOTOPEN: &str = "fid not open for requested operation";
pub const EBADWNAME: &str = "bad walk name";
pub const EBADMODE: &str = "invalid open mode";
pub const EBADDIROFFSET: &str = "invalid directory offset";
pub const EBADMSIZE: &str = "version: message size too small";
pub const ENOAUTH: &str = "authentication not required";
pub const EWSTATTYPE: &str = "wstat -- attempt to change type";
pub const EWSTATDEV: &str = "wstat -- attempt to change dev";
pub const EWSTATQID: &str = "wstat -- attempt to change qid";
pub const EWSTATATIME: &str = "wstat -- attempt to change atime";
pub const EWSTATUID: &str = "wstat -- attempt to change uid";
pub const EWSTATMUID: &str = "wstat -- attempt to change muid";
pub const EWSTATDMDIR: &str = "wstat -- attempt to change DMDIR bit";
pub const EWSTATDMSYMLINK: &str = "wstat -- attempt to change DMSYMLINK bit";
pub const EWSTATDIRLENGTH: &str = "wstat -- attempt to set non-zero directory length";
pub const ESYMLINKDIALECT: &str = "symlink metadata requires negotiated 9P2000.r9p";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    message: Vec<u8>,
}

impl Error {
    pub fn new(message: impl Into<Vec<u8>>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn from_static(message: &'static str) -> Self {
        Self::new(message.as_bytes().to_vec())
    }

    pub fn message(&self) -> &[u8] {
        &self.message
    }

    pub fn into_message(self) -> Vec<u8> {
        self.message
    }

    pub fn display_lossy(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.message)
    }
}

impl From<&'static str> for Error {
    fn from(value: &'static str) -> Self {
        Self::from_static(value)
    }
}

impl From<String> for Error {
    fn from(value: String) -> Self {
        Self::new(value.into_bytes())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display_lossy())
    }
}

impl std::error::Error for Error {}
