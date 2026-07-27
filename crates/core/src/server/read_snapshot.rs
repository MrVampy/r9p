use crate::{
    error::{Error, Result, EBADFID},
    fid::Fid,
};
use std::collections::BTreeMap;

/// Bounded byte snapshots keyed by an opened fid.
///
/// A backend chooses which ordinary files use snapshot semantics. Live files
/// such as waits and streams stay outside this store.
#[derive(Debug)]
pub struct FidReadSnapshots {
    snapshots: BTreeMap<Fid, Vec<u8>>,
    snapshot_bytes: usize,
    max_bytes: usize,
}

impl FidReadSnapshots {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            snapshots: BTreeMap::new(),
            snapshot_bytes: 0,
            max_bytes,
        }
    }

    pub fn contains(&self, fid: Fid) -> bool {
        self.snapshots.contains_key(&fid)
    }

    pub fn capture(&mut self, fid: Fid, bytes: Vec<u8>) -> Result<()> {
        if self.contains(fid) {
            return Err(Error::from_static("read snapshot already captured"));
        }
        let projected_bytes = self
            .snapshot_bytes
            .checked_add(bytes.len())
            .filter(|projected| *projected <= self.max_bytes)
            .ok_or_else(|| Error::from_static("read snapshot capacity exhausted"))?;
        self.snapshot_bytes = projected_bytes;
        self.snapshots.insert(fid, bytes);
        Ok(())
    }

    pub fn read(&self, fid: Fid, offset: u64, count: u32) -> Result<Vec<u8>> {
        let bytes = self
            .snapshots
            .get(&fid)
            .ok_or_else(|| Error::from_static(EBADFID))?;
        let offset =
            usize::try_from(offset).map_err(|_| Error::from_static("read offset too large"))?;
        if offset >= bytes.len() || count == 0 {
            return Ok(Vec::new());
        }
        let count =
            usize::try_from(count).map_err(|_| Error::from_static("read count too large"))?;
        let end = offset.saturating_add(count).min(bytes.len());
        Ok(bytes[offset..end].to_vec())
    }

    pub fn remove(&mut self, fid: Fid) {
        if let Some(bytes) = self.snapshots.remove(&fid) {
            self.snapshot_bytes = self.snapshot_bytes.saturating_sub(bytes.len());
        }
    }

    pub fn clear(&mut self) {
        self.snapshots.clear();
        self.snapshot_bytes = 0;
    }

    pub fn len(&self) -> usize {
        self.snapshots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.snapshot_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_fid_reads_one_coherent_snapshot() -> Result<()> {
        let mut snapshots = FidReadSnapshots::new(64);
        snapshots.capture(7, b"first version".to_vec())?;

        assert_eq!(snapshots.read(7, 0, 6)?, b"first");
        assert_eq!(snapshots.read(7, 6, 64)?, b" version");
        assert_eq!(snapshots.read(7, 13, 1)?, b"");
        assert_eq!(snapshots.read(7, 0, 64)?, b"first version");
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots.bytes(), 13);
        Ok(())
    }

    #[test]
    fn empty_snapshot_is_still_a_captured_snapshot() -> Result<()> {
        let mut snapshots = FidReadSnapshots::new(0);
        snapshots.capture(2, Vec::new())?;

        assert!(snapshots.contains(2));
        assert_eq!(snapshots.read(2, 0, 1)?, b"");
        assert_eq!(snapshots.bytes(), 0);
        Ok(())
    }

    #[test]
    fn capacity_failure_does_not_mutate_existing_snapshots() -> Result<()> {
        let mut snapshots = FidReadSnapshots::new(4);
        snapshots.capture(1, b"abc".to_vec())?;

        assert!(snapshots.capture(2, b"de".to_vec()).is_err());
        assert!(!snapshots.contains(2));
        assert_eq!(snapshots.read(1, 0, 4)?, b"abc");
        assert_eq!(snapshots.bytes(), 3);
        Ok(())
    }

    #[test]
    fn fid_retirement_releases_capacity() -> Result<()> {
        let mut snapshots = FidReadSnapshots::new(3);
        snapshots.capture(1, b"abc".to_vec())?;
        snapshots.remove(1);
        snapshots.capture(2, b"def".to_vec())?;

        assert!(!snapshots.contains(1));
        assert_eq!(snapshots.read(2, 0, 3)?, b"def");
        assert_eq!(snapshots.bytes(), 3);
        snapshots.clear();
        assert!(snapshots.is_empty());
        assert_eq!(snapshots.bytes(), 0);
        Ok(())
    }
}
