use crate::{
    codec::MIN_MSIZE,
    error::{Error, Result, EBADFID, EBADMSIZE, EFIDBUSY, EFIDINUSE, EFIDLIMIT},
    fid::{Fid, FidState},
    flush::{RequestKey, RequestTable},
};
use std::collections::{BTreeMap, BTreeSet};

use super::config::ServerConfig;

#[derive(Debug)]
pub struct Session {
    pub(super) config: ServerConfig,
    msize: u32,
    version: Vec<u8>,
    negotiated: bool,
    fids: BTreeMap<Fid, FidState>,
    reservations: BTreeMap<Fid, FidReservations>,
    pub(super) requests: RequestTable,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct FidReservation {
    request: RequestKey,
    new_fid: bool,
}

#[derive(Debug, Default)]
struct FidReservations {
    exclusive: Option<FidReservation>,
    shared: BTreeSet<RequestKey>,
}

impl FidReservations {
    fn is_empty(&self) -> bool {
        self.exclusive.is_none() && self.shared.is_empty()
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum VersionNegotiation {
    Accepted,
    Unknown,
}

impl Session {
    pub fn new(config: ServerConfig) -> Self {
        Self {
            msize: config.default_msize,
            version: b"unknown".to_vec(),
            negotiated: false,
            fids: BTreeMap::new(),
            reservations: BTreeMap::new(),
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

    pub fn is_negotiated(&self) -> bool {
        self.negotiated
    }

    pub fn fid_count(&self) -> usize {
        self.fids.len()
            + self
                .reservations
                .values()
                .filter(|reservations| {
                    reservations
                        .exclusive
                        .is_some_and(|reservation| reservation.new_fid)
                })
                .count()
    }

    pub fn contains_fid(&self, fid: Fid) -> bool {
        self.fids.contains_key(&fid)
            || self.reservations.get(&fid).is_some_and(|reservations| {
                reservations
                    .exclusive
                    .is_some_and(|reservation| reservation.new_fid)
            })
    }

    pub fn reset_for_version(
        &mut self,
        requested_msize: u32,
        version: &[u8],
    ) -> Result<VersionNegotiation> {
        self.invalidate();
        if requested_msize < MIN_MSIZE {
            return Err(Error::from_static(EBADMSIZE));
        }
        match self.config.variant.accept(version) {
            Some(accepted) => {
                self.msize = requested_msize.min(self.config.max_msize);
                self.version = accepted.wire_name().to_vec();
                self.negotiated = true;
                Ok(VersionNegotiation::Accepted)
            }
            None => Ok(VersionNegotiation::Unknown),
        }
    }

    pub(super) fn invalidate(&mut self) {
        self.fids.clear();
        self.reservations.clear();
        self.requests.reset();
        self.msize = MIN_MSIZE;
        self.version = b"unknown".to_vec();
        self.negotiated = false;
    }

    pub(super) fn bind_fid(&mut self, fid: Fid, state: FidState) -> Result<()> {
        self.ensure_unreserved(fid)?;
        if !self.fids.contains_key(&fid) && self.fids.len() >= self.config.max_fids {
            return Err(Error::from_static(EFIDLIMIT));
        }
        self.fids.insert(fid, state);
        Ok(())
    }

    pub(super) fn insert_new_fid(&mut self, fid: Fid, state: FidState) -> Result<()> {
        if self.fids.contains_key(&fid) {
            return Err(Error::from_static(EFIDINUSE));
        }
        self.bind_fid(fid, state)
    }

    pub(super) fn retire_fid(&mut self, request: RequestKey, fid: Fid) -> Result<FidState> {
        self.ensure_unreserved(fid)?;
        let state = self
            .fids
            .remove(&fid)
            .ok_or_else(|| Error::from_static(EBADFID))?;
        self.reservations.insert(
            fid,
            FidReservations {
                exclusive: Some(FidReservation {
                    request,
                    new_fid: false,
                }),
                shared: BTreeSet::new(),
            },
        );
        Ok(state)
    }

    pub fn fid(&self, fid: Fid) -> Result<FidState> {
        self.ensure_available(fid)?;
        self.fids
            .get(&fid)
            .copied()
            .ok_or_else(|| Error::from_static(EBADFID))
    }

    pub(super) fn reserve_existing_fid(&mut self, request: RequestKey, fid: Fid) -> Result<()> {
        if !self.fids.contains_key(&fid) {
            return Err(Error::from_static(EBADFID));
        }
        let reservations = self.reservations.entry(fid).or_default();
        if !reservations.is_empty() {
            return Err(Error::from_static(EFIDBUSY));
        }
        reservations.exclusive = Some(FidReservation {
            request,
            new_fid: false,
        });
        Ok(())
    }

    pub(super) fn reserve_shared_fid(&mut self, request: RequestKey, fid: Fid) -> Result<()> {
        if !self.fids.contains_key(&fid) {
            return Err(Error::from_static(EBADFID));
        }
        let reservations = self.reservations.entry(fid).or_default();
        if reservations.exclusive.is_some() {
            return Err(Error::from_static(EFIDBUSY));
        }
        reservations.shared.insert(request);
        Ok(())
    }

    pub(super) fn reserve_new_fid(&mut self, request: RequestKey, fid: Fid) -> Result<()> {
        if self.contains_fid(fid) || self.reservations.contains_key(&fid) {
            return Err(Error::from_static(EFIDINUSE));
        }
        if self.fid_count() >= self.config.max_fids {
            return Err(Error::from_static(EFIDLIMIT));
        }
        self.reservations.insert(
            fid,
            FidReservations {
                exclusive: Some(FidReservation {
                    request,
                    new_fid: true,
                }),
                shared: BTreeSet::new(),
            },
        );
        Ok(())
    }

    pub(super) fn release_reservations(&mut self, request: RequestKey) {
        for reservations in self.reservations.values_mut() {
            if reservations
                .exclusive
                .is_some_and(|reservation| reservation.request == request)
            {
                reservations.exclusive = None;
            }
            reservations.shared.remove(&request);
        }
        self.reservations
            .retain(|_, reservations| !reservations.is_empty());
    }

    fn ensure_available(&self, fid: Fid) -> Result<()> {
        if self
            .reservations
            .get(&fid)
            .is_some_and(|reservations| reservations.exclusive.is_some())
        {
            Err(Error::from_static(EFIDBUSY))
        } else {
            Ok(())
        }
    }

    fn ensure_unreserved(&self, fid: Fid) -> Result<()> {
        if self
            .reservations
            .get(&fid)
            .is_some_and(|reservations| !reservations.is_empty())
        {
            Err(Error::from_static(EFIDBUSY))
        } else {
            Ok(())
        }
    }

    pub fn request_table(&mut self) -> &mut RequestTable {
        &mut self.requests
    }
}
