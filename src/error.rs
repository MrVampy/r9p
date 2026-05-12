use std::{borrow::Cow, fmt};

pub type Result<T> = std::result::Result<T, Error>;

pub const EPERM: &str = "permission denied";
pub const EEXIST: &str = "file does not exist";
pub const ENOTDIR: &str = "not a directory";
pub const EDEL: &str = "deleted window";
pub const EBADCTL: &str = "ill-formed control message";
pub const EINUSE: &str = "already in use";

pub const EBADTAG: &str = "invalid tag";
pub const EDUPTAG: &str = "duplicate tag";
pub const EBADFID: &str = "unknown fid";
pub const EFIDINUSE: &str = "fid already in use";
pub const EFIDLIMIT: &str = "fid limit exceeded";
pub const EBADWNAME: &str = "bad walk name";
pub const EBADMSIZE: &str = "version: message size too small";
pub const EBADVERSION: &str = "unrecognized 9P version";
pub const ENOAUTH: &str = "acme: authentication not required";

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
