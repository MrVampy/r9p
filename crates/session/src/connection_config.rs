use crate::AuthorityBindings;
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectionConfig {
    pub address: String,
    pub uname: String,
    pub aname: String,
    pub msize: u32,
    pub auth_config: Option<PathBuf>,
    pub authorities: AuthorityBindings,
}
