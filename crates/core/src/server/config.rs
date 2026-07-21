use crate::codec::{Variant, DEFAULT_MSIZE, MAX_MSIZE};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    pub default_msize: u32,
    pub max_msize: u32,
    pub max_fids: usize,
    pub max_async_requests: usize,
    pub variant: Variant,
    /// When present, the transport has authenticated this user name and every
    /// Tauth/Tattach on the connection must claim exactly the same identity.
    pub session_uname: Option<Vec<u8>>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            default_msize: DEFAULT_MSIZE,
            max_msize: MAX_MSIZE,
            max_fids: 4096,
            max_async_requests: 256,
            variant: Variant::Plain,
            session_uname: None,
        }
    }
}
