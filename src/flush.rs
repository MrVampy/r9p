use crate::{
    error::{Error, Result, EBADTAG, EDUPTAG},
    message::{Tag, NOTAG},
};
use std::collections::BTreeMap;

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestKey {
    pub tag: Tag,
    pub generation: u64,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum FlushOutcome {
    Cancelled(RequestKey),
    Unknown,
}

#[derive(Debug, Default)]
pub struct RequestTable {
    next_generation: u64,
    live: BTreeMap<Tag, RequestKey>,
}

impl RequestTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_live(&self, tag: Tag) -> bool {
        self.live.contains_key(&tag)
    }

    pub fn begin(&mut self, tag: Tag) -> Result<RequestKey> {
        if tag == NOTAG {
            return Err(Error::from_static(EBADTAG));
        }
        if self.live.contains_key(&tag) {
            return Err(Error::from_static(EDUPTAG));
        }
        let key = RequestKey {
            tag,
            generation: self.next_generation,
        };
        self.next_generation = self.next_generation.saturating_add(1);
        self.live.insert(tag, key);
        Ok(key)
    }

    pub fn finish(&mut self, key: RequestKey) -> bool {
        match self.live.get(&key.tag) {
            Some(current) if *current == key => {
                self.live.remove(&key.tag);
                true
            }
            _ => false,
        }
    }

    pub fn flush(&mut self, oldtag: Tag) -> Result<FlushOutcome> {
        if oldtag == NOTAG {
            return Err(Error::from_static(EBADTAG));
        }
        Ok(match self.live.remove(&oldtag) {
            Some(key) => FlushOutcome::Cancelled(key),
            None => FlushOutcome::Unknown,
        })
    }

    pub fn reset(&mut self) {
        self.live.clear();
        self.next_generation = self.next_generation.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_live_tags_are_rejected() -> Result<()> {
        let mut table = RequestTable::new();
        let _first = table.begin(7)?;
        let err = table.begin(7).err().ok_or("duplicate tag accepted")?;
        assert_eq!(err.message(), EDUPTAG.as_bytes());
        Ok(())
    }

    #[test]
    fn tag_reuse_after_reply_is_allowed() -> Result<()> {
        let mut table = RequestTable::new();
        let first = table.begin(7)?;
        assert!(table.finish(first));
        let second = table.begin(7)?;
        assert_ne!(first, second);
        Ok(())
    }

    #[test]
    fn flush_unknown_tag_is_successful() -> Result<()> {
        let mut table = RequestTable::new();
        assert_eq!(table.flush(9)?, FlushOutcome::Unknown);
        Ok(())
    }

    #[test]
    fn flush_notag_is_rejected() -> Result<()> {
        let mut table = RequestTable::new();
        let err = table.flush(NOTAG).err().ok_or("NOTAG flush accepted")?;
        assert_eq!(err.message(), EBADTAG.as_bytes());
        Ok(())
    }

    #[test]
    fn stale_completion_after_flush_is_rejected() -> Result<()> {
        let mut table = RequestTable::new();
        let old = table.begin(1)?;
        assert_eq!(table.flush(1)?, FlushOutcome::Cancelled(old));
        let new = table.begin(1)?;
        assert!(!table.finish(old));
        assert!(table.finish(new));
        Ok(())
    }
}
