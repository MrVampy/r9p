use crate::codec::{DEFAULT_MSIZE, MAX_MSIZE};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    pub default_msize: u32,
    pub max_msize: u32,
    pub max_fids: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            default_msize: DEFAULT_MSIZE,
            max_msize: MAX_MSIZE,
            max_fids: 4096,
        }
    }
}
