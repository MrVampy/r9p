use crate::{Client, Error, Result};
use r9p::{
    blocking::DEFAULT_READ_CHUNK,
    fid::Fid,
    qid::{Qid, DMDIR, DMSYMLINK, QTDIR, QTSYMLINK},
    stat::{decode_dir_entries as decode_p9_dir_entries, Stat},
};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaleReason {
    Reconnect,
    NamespaceChange,
    IdentityChange,
    NotDirectory,
    Explicit(String),
}

#[derive(Debug, Clone)]
pub struct Freshness {
    observed_at: Instant,
    stale_reason: Option<StaleReason>,
}

impl Freshness {
    pub fn fresh_now() -> Self {
        Self {
            observed_at: Instant::now(),
            stale_reason: None,
        }
    }

    pub fn stale_now(reason: StaleReason) -> Self {
        Self {
            observed_at: Instant::now(),
            stale_reason: Some(reason),
        }
    }

    pub fn observed_at(&self) -> Instant {
        self.observed_at
    }

    pub fn stale_reason(&self) -> Option<&StaleReason> {
        self.stale_reason.as_ref()
    }

    pub fn is_stale(&self) -> bool {
        self.stale_reason.is_some()
    }

    pub fn mark_fresh(&mut self) {
        self.observed_at = Instant::now();
        self.stale_reason = None;
    }

    pub fn mark_stale(&mut self, reason: StaleReason) {
        self.observed_at = Instant::now();
        self.stale_reason = Some(reason);
    }

    pub fn age_at(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.observed_at)
    }

    pub fn is_fresh_at(&self, now: Instant, ttl: Duration) -> bool {
        !ttl.is_zero() && !self.is_stale() && self.age_at(now) <= ttl
    }
}

#[derive(Debug, Clone)]
pub struct DirCache {
    pub entries: Vec<DirEntry>,
    pub freshness: Freshness,
}

impl DirCache {
    pub fn fresh(entries: Vec<DirEntry>) -> Self {
        Self {
            entries,
            freshness: Freshness::fresh_now(),
        }
    }

    pub fn is_fresh_at(&self, now: Instant, ttl: Duration) -> bool {
        self.freshness.is_fresh_at(now, ttl)
    }
}

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: Vec<u8>,
    pub qid: Qid,
    pub stat: Stat,
}

pub fn read_open_directory_entries(
    client: &Client,
    fid: Fid,
    timeout: Duration,
) -> Result<Vec<DirEntry>> {
    let mut offset = 0_u64;
    let mut all = Vec::new();
    loop {
        let chunk = client.read_timeout(fid, offset, DEFAULT_READ_CHUNK, timeout)?;
        if chunk.is_empty() {
            break;
        }
        offset = offset.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        all.extend(chunk);
    }
    decode_dir_entries(&all)
}

pub fn decode_dir_entries(data: &[u8]) -> Result<Vec<DirEntry>> {
    let entries = decode_p9_dir_entries(data)
        .map_err(|error| Error::new(libc::EPROTO, format!("decode dir stat: {error}")))?
        .into_iter()
        .map(|stat| DirEntry {
            name: stat.name.clone(),
            qid: stat.qid,
            stat,
        })
        .collect::<Vec<_>>();
    Ok(entries)
}

pub fn is_dir(stat: &Stat) -> bool {
    stat.qid.qtype & QTDIR != 0 || stat.mode & DMDIR != 0
}

pub fn is_symlink(stat: &Stat) -> bool {
    stat.qid.qtype & QTSYMLINK != 0 || stat.mode & DMSYMLINK != 0
}

pub fn same_qid(a: Qid, b: Qid) -> bool {
    a.path == b.path && a.version == b.version && a.qtype == b.qtype
}

pub fn null_wstat() -> Stat {
    Stat {
        type_: u16::MAX,
        dev: u32::MAX,
        qid: Qid::new(u8::MAX, u32::MAX, u64::MAX),
        mode: u32::MAX,
        atime: u32::MAX,
        mtime: u32::MAX,
        length: u64::MAX,
        name: Vec::new(),
        uid: Vec::new(),
        gid: Vec::new(),
        muid: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::{is_dir, same_qid, DirCache, DirEntry, Freshness, StaleReason};
    use r9p::{qid::Qid, stat::Stat};
    use std::time::{Duration, Instant};

    fn is_fresh_at(freshness: &Freshness, now: Instant, ttl: Duration) -> bool {
        freshness.is_fresh_at(now, ttl)
    }

    #[test]
    fn freshness_requires_nonzero_ttl_and_no_stale_reason() {
        let freshness = Freshness::fresh_now();
        let now = freshness.observed_at();

        assert!(is_fresh_at(&freshness, now, Duration::from_secs(1)));
        assert!(!is_fresh_at(&freshness, now, Duration::ZERO));

        let stale = Freshness::stale_now(StaleReason::NamespaceChange);
        assert!(!is_fresh_at(
            &stale,
            stale.observed_at(),
            Duration::from_secs(1)
        ));
    }

    #[test]
    fn directory_cache_carries_freshness() {
        let entry = DirEntry {
            name: b"alpha".to_vec(),
            qid: Qid::file(7),
            stat: Stat::new("alpha", Qid::file(7), 0o444),
        };
        let cache = DirCache::fresh(vec![entry]);

        assert_eq!(cache.entries.len(), 1);
        assert!(cache.is_fresh_at(cache.freshness.observed_at(), Duration::from_secs(1)));
    }

    #[test]
    fn stat_predicates_follow_9p_qid_and_mode_bits() {
        assert!(is_dir(&Stat::new("docs", Qid::dir(1), 0)));
        assert!(is_dir(&Stat::new("docs", Qid::file(1), r9p::qid::DMDIR)));
        assert!(same_qid(Qid::file(1), Qid::file(1)));
        assert!(!same_qid(Qid::file(1), Qid::file(2)));
    }
}
