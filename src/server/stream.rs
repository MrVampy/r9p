use crate::{
    error::{Error, Result, EBADFID},
    fid::Fid,
};
use std::collections::{BTreeMap, VecDeque};

pub trait Stream {
    fn length(&self) -> u64;
    fn add_reader(&mut self, fid: Fid) -> Result<()>;
    fn remove_reader(&mut self, fid: Fid);
    fn read(&mut self, fid: Fid, offset: u64, count: u32) -> Result<Vec<u8>>;
}

pub struct Broadcaster {
    readers: BTreeMap<Fid, VecDeque<u8>>,
    total_pushed: u64,
}

impl Broadcaster {
    pub fn new() -> Self {
        Self {
            readers: BTreeMap::new(),
            total_pushed: 0,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        for queue in self.readers.values_mut() {
            queue.extend(bytes);
        }
        self.total_pushed = self.total_pushed.saturating_add(bytes.len() as u64);
    }

    pub fn reader_count(&self) -> usize {
        self.readers.len()
    }

    pub fn pending(&self, fid: Fid) -> Option<usize> {
        self.readers.get(&fid).map(|q| q.len())
    }
}

impl Default for Broadcaster {
    fn default() -> Self {
        Self::new()
    }
}

impl Stream for Broadcaster {
    fn length(&self) -> u64 {
        self.total_pushed
    }

    fn add_reader(&mut self, fid: Fid) -> Result<()> {
        self.readers.insert(fid, VecDeque::new());
        Ok(())
    }

    fn remove_reader(&mut self, fid: Fid) {
        self.readers.remove(&fid);
    }

    fn read(&mut self, fid: Fid, _offset: u64, count: u32) -> Result<Vec<u8>> {
        let queue = self
            .readers
            .get_mut(&fid)
            .ok_or_else(|| Error::from_static(EBADFID))?;
        let take = (count as usize).min(queue.len());
        Ok(queue.drain(..take).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broadcaster_fans_pushed_bytes_to_each_reader_independently() -> Result<()> {
        let mut b = Broadcaster::new();
        b.add_reader(1)?;
        b.add_reader(2)?;
        b.push(b"hello");
        assert_eq!(b.length(), 5);
        assert_eq!(b.read(1, 0, 5)?, b"hello");
        assert_eq!(b.read(2, 0, 5)?, b"hello");
        assert_eq!(b.read(1, 0, 5)?, b"");
        Ok(())
    }

    #[test]
    fn broadcaster_read_clamps_to_pending() -> Result<()> {
        let mut b = Broadcaster::new();
        b.add_reader(7)?;
        b.push(b"abc");
        assert_eq!(b.read(7, 0, 100)?, b"abc");
        Ok(())
    }

    #[test]
    fn broadcaster_remove_reader_drops_pending() -> Result<()> {
        let mut b = Broadcaster::new();
        b.add_reader(3)?;
        b.push(b"data");
        assert_eq!(b.pending(3), Some(4));
        b.remove_reader(3);
        assert_eq!(b.pending(3), None);
        assert!(b.read(3, 0, 4).is_err());
        Ok(())
    }

    #[test]
    fn broadcaster_pushes_after_reader_joined_skip_earlier_pushes() -> Result<()> {
        let mut b = Broadcaster::new();
        b.add_reader(1)?;
        b.push(b"first");
        b.add_reader(2)?;
        b.push(b"second");
        assert_eq!(b.read(1, 0, 11)?, b"firstsecond");
        assert_eq!(b.read(2, 0, 11)?, b"second");
        Ok(())
    }
}
