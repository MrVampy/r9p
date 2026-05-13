use crate::{
    codec::MIN_MSIZE,
    error::{Error, Result, EBADFID, EBADMSIZE, EBADVERSION, EFIDINUSE, EFIDLIMIT},
    fid::{Fid, FidState},
    flush::RequestTable,
};
use std::collections::BTreeMap;

use super::config::ServerConfig;

#[derive(Debug)]
pub struct Session {
    pub(super) config: ServerConfig,
    msize: u32,
    version: Vec<u8>,
    fids: BTreeMap<Fid, FidState>,
    pub(super) requests: RequestTable,
}

impl Session {
    pub fn new(config: ServerConfig) -> Self {
        Self {
            msize: config.default_msize,
            version: b"9P2000".to_vec(),
            fids: BTreeMap::new(),
            requests: RequestTable::new(),
            config,
        }
    }

    pub fn msize(&self) -> u32 {
        self.msize
    }

    pub fn version(&self) -> &[u8] {
        &self.version
    }

    pub fn fid_count(&self) -> usize {
        self.fids.len()
    }

    pub fn contains_fid(&self, fid: Fid) -> bool {
        self.fids.contains_key(&fid)
    }

    pub fn reset_for_version(&mut self, requested_msize: u32, version: &[u8]) -> Result<()> {
        self.fids.clear();
        self.requests.reset();
        if requested_msize < MIN_MSIZE {
            return Err(Error::from_static(EBADMSIZE));
        }
        if !version.starts_with(b"9P2000") {
            return Err(Error::from_static(EBADVERSION));
        }
        self.msize = requested_msize.min(self.config.max_msize);
        self.version = b"9P2000".to_vec();
        Ok(())
    }

    pub fn bind_fid(&mut self, fid: Fid, state: FidState) -> Result<()> {
        if !self.fids.contains_key(&fid) && self.fids.len() >= self.config.max_fids {
            return Err(Error::from_static(EFIDLIMIT));
        }
        self.fids.insert(fid, state);
        Ok(())
    }

    pub fn insert_new_fid(&mut self, fid: Fid, state: FidState) -> Result<()> {
        if self.fids.contains_key(&fid) {
            return Err(Error::from_static(EFIDINUSE));
        }
        self.bind_fid(fid, state)
    }

    pub fn remove_fid(&mut self, fid: Fid) -> Result<FidState> {
        self.fids
            .remove(&fid)
            .ok_or_else(|| Error::from_static(EBADFID))
    }

    pub fn fid(&self, fid: Fid) -> Result<FidState> {
        self.fids
            .get(&fid)
            .copied()
            .ok_or_else(|| Error::from_static(EBADFID))
    }

    pub fn request_table(&mut self) -> &mut RequestTable {
        &mut self.requests
    }
}
