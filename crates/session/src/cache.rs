use crate::{Client, Error, Result};
use r9p::{
    blocking::DEFAULT_READ_CHUNK,
    fid::Fid,
    qid::{Qid, DMDIR, DMSYMLINK, QTDIR, QTSYMLINK},
    stat::{decode_dir_entries as decode_p9_dir_entries, Stat},
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

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

#[derive(Clone, Debug)]
pub struct NamespaceCache {
    inner: Arc<Mutex<NamespaceCacheInner>>,
}

#[derive(Debug, Default)]
struct NamespaceCacheInner {
    entries: BTreeMap<Vec<Vec<u8>>, CachedNode>,
}

#[derive(Debug, Clone)]
struct CachedNode {
    stat: Stat,
    stat_freshness: Freshness,
    dir_cache: Option<DirCache>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NamespaceCacheStats {
    pub entries: usize,
    pub directories: usize,
    pub stale_entries: usize,
}

impl NamespaceCache {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(NamespaceCacheInner::default())),
        }
    }

    pub fn stat_if_fresh(&self, path: &[Vec<u8>]) -> Option<Stat> {
        self.inner
            .lock()
            .ok()
            .and_then(|inner| inner.entries.get(path).cloned())
            .filter(|node| !node.stat_freshness.is_stale())
            .map(|node| node.stat)
    }

    pub fn directory_if_fresh(&self, path: &[Vec<u8>]) -> Option<Vec<DirEntry>> {
        self.inner
            .lock()
            .ok()
            .and_then(|inner| inner.entries.get(path).cloned())
            .filter(|node| !node.stat_freshness.is_stale())
            .and_then(|node| node.dir_cache)
            .filter(|cache| !cache.freshness.is_stale())
            .map(|cache| cache.entries)
    }

    pub fn update_stat(&self, path: &[Vec<u8>], stat: Stat) {
        if let Ok(mut inner) = self.inner.lock() {
            let entry = inner
                .entries
                .entry(path.to_vec())
                .or_insert_with(|| CachedNode {
                    stat: stat.clone(),
                    stat_freshness: Freshness::fresh_now(),
                    dir_cache: None,
                });
            let identity_changed = !same_qid(entry.stat.qid, stat.qid);
            entry.stat = stat;
            entry.stat_freshness.mark_fresh();
            if identity_changed || !is_dir(&entry.stat) {
                entry.dir_cache = None;
            }
        }
    }

    pub fn update_directory(&self, path: &[Vec<u8>], entries: Vec<DirEntry>) {
        if let Ok(mut inner) = self.inner.lock() {
            let Some(entry) = inner.entries.get_mut(path) else {
                return;
            };
            if is_dir(&entry.stat) && !entry.stat_freshness.is_stale() {
                entry.dir_cache = Some(DirCache::fresh(entries.clone()));
                seed_child_stats(&mut inner, path, entries);
            }
        }
    }

    pub fn mark_namespace_change(&self, path: &str, old_path: Option<&str>) {
        match parse_absolute_path(path) {
            Some(segments) => self.mark_path_stale(&segments, StaleReason::NamespaceChange),
            None => self.mark_all_stale(StaleReason::Explicit(format!(
                "invalid namespace change path {path}"
            ))),
        }
        if let Some(old_path) = old_path {
            match parse_absolute_path(old_path) {
                Some(segments) => self.mark_path_stale(&segments, StaleReason::NamespaceChange),
                None => self.mark_all_stale(StaleReason::Explicit(format!(
                    "invalid namespace change old_path {old_path}"
                ))),
            }
        }
    }

    pub fn mark_path_stale(&self, path: &[Vec<u8>], reason: StaleReason) {
        if let Ok(mut inner) = self.inner.lock() {
            if let Some(parent) = parent_path(path) {
                if let Some(parent_node) = inner.entries.get_mut(parent) {
                    parent_node.dir_cache = None;
                }
            }
            for (entry_path, node) in inner.entries.iter_mut() {
                if path_is_prefix(path, entry_path) {
                    node.stat_freshness.mark_stale(reason.clone());
                    node.dir_cache = None;
                }
            }
        }
    }

    pub fn mark_all_stale(&self, reason: StaleReason) {
        if let Ok(mut inner) = self.inner.lock() {
            for node in inner.entries.values_mut() {
                node.stat_freshness.mark_stale(reason.clone());
                node.dir_cache = None;
            }
        }
    }

    pub fn stats(&self) -> NamespaceCacheStats {
        self.inner
            .lock()
            .map(|inner| NamespaceCacheStats {
                entries: inner.entries.len(),
                directories: inner
                    .entries
                    .values()
                    .filter(|node| node.dir_cache.is_some())
                    .count(),
                stale_entries: inner
                    .entries
                    .values()
                    .filter(|node| node.stat_freshness.is_stale())
                    .count(),
            })
            .unwrap_or(NamespaceCacheStats {
                entries: 0,
                directories: 0,
                stale_entries: 0,
            })
    }
}

impl Default for NamespaceCache {
    fn default() -> Self {
        Self::new()
    }
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
    validate_directory_entries(client, &all)
}

pub fn validate_directory_entries(client: &Client, data: &[u8]) -> Result<Vec<DirEntry>> {
    decode_dir_entries(data)?
        .into_iter()
        .map(|entry| {
            client.validate_stat(entry.stat).map(|stat| DirEntry {
                name: stat.name.clone(),
                qid: stat.qid,
                stat,
            })
        })
        .collect()
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

fn parent_path(path: &[Vec<u8>]) -> Option<&[Vec<u8>]> {
    if path.is_empty() {
        None
    } else {
        Some(&path[..path.len() - 1])
    }
}

fn path_is_prefix(prefix: &[Vec<u8>], path: &[Vec<u8>]) -> bool {
    prefix.len() <= path.len() && path.iter().zip(prefix).all(|(path, prefix)| path == prefix)
}

fn seed_child_stats(inner: &mut NamespaceCacheInner, parent: &[Vec<u8>], entries: Vec<DirEntry>) {
    for child in entries {
        if child.name == b"." || child.name == b".." {
            continue;
        }
        let mut child_path = parent.to_vec();
        child_path.push(child.name);
        let entry = inner
            .entries
            .entry(child_path)
            .or_insert_with(|| CachedNode {
                stat: child.stat.clone(),
                stat_freshness: Freshness::fresh_now(),
                dir_cache: None,
            });
        let identity_changed = !same_qid(entry.stat.qid, child.stat.qid);
        entry.stat = child.stat;
        entry.stat_freshness.mark_fresh();
        if identity_changed || !is_dir(&entry.stat) {
            entry.dir_cache = None;
        }
    }
}

fn parse_absolute_path(path: &str) -> Option<Vec<Vec<u8>>> {
    path.starts_with('/').then(|| {
        path.split('/')
            .filter(|segment| !segment.is_empty())
            .map(|segment| segment.as_bytes().to_vec())
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::{is_dir, same_qid, DirCache, DirEntry, Freshness, NamespaceCache, StaleReason};
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

    #[test]
    fn namespace_cache_invalidates_changed_path_and_parent_directory() {
        let cache = NamespaceCache::new();
        let root = Vec::<Vec<u8>>::new();
        let docs = vec![b"docs".to_vec()];
        let alpha = vec![b"docs".to_vec(), b"alpha".to_vec()];

        cache.update_stat(&root, Stat::new("", Qid::dir(1), 0o555));
        cache.update_stat(&docs, Stat::new("docs", Qid::dir(2), 0o555));
        cache.update_directory(
            &docs,
            vec![DirEntry {
                name: b"alpha".to_vec(),
                qid: Qid::file(3),
                stat: Stat::new("alpha", Qid::file(3), 0o444),
            }],
        );
        cache.update_stat(&alpha, Stat::new("alpha", Qid::file(3), 0o444));

        assert!(cache.directory_if_fresh(&docs).is_some());
        assert!(cache.stat_if_fresh(&alpha).is_some());

        cache.mark_namespace_change("/docs/alpha", None);

        assert!(cache.directory_if_fresh(&docs).is_none());
        assert!(cache.stat_if_fresh(&alpha).is_none());
        assert!(cache.stat_if_fresh(&root).is_some());
    }

    #[test]
    fn directory_cache_update_seeds_child_stat_entries() {
        let cache = NamespaceCache::new();
        let root = Vec::<Vec<u8>>::new();
        let docs = vec![b"docs".to_vec()];
        let alpha = vec![b"docs".to_vec(), b"alpha".to_vec()];

        cache.update_stat(&root, Stat::new("", Qid::dir(1), 0o555));
        cache.update_directory(
            &root,
            vec![DirEntry {
                name: b"docs".to_vec(),
                qid: Qid::dir(2),
                stat: Stat::new("docs", Qid::dir(2), 0o555),
            }],
        );

        assert!(cache.stat_if_fresh(&docs).is_some());

        cache.update_directory(
            &docs,
            vec![DirEntry {
                name: b"alpha".to_vec(),
                qid: Qid::file(3),
                stat: Stat::new("alpha", Qid::file(3), 0o444),
            }],
        );

        assert!(cache.stat_if_fresh(&alpha).is_some());
    }

    #[test]
    fn directory_cache_update_clears_replaced_child_directory_cache() {
        let cache = NamespaceCache::new();
        let root = Vec::<Vec<u8>>::new();
        let docs = vec![b"docs".to_vec()];

        cache.update_stat(&root, Stat::new("", Qid::dir(1), 0o555));
        cache.update_stat(&docs, Stat::new("docs", Qid::dir(2), 0o555));
        cache.update_directory(&docs, Vec::new());
        assert!(cache.directory_if_fresh(&docs).is_some());

        cache.update_directory(
            &root,
            vec![DirEntry {
                name: b"docs".to_vec(),
                qid: Qid::file(3),
                stat: Stat::new("docs", Qid::file(3), 0o444),
            }],
        );

        assert!(cache.stat_if_fresh(&docs).is_some());
        assert!(cache.directory_if_fresh(&docs).is_none());
    }

    #[test]
    fn namespace_cache_invalidates_renamed_old_and_new_paths() {
        let cache = NamespaceCache::new();
        let old_path = vec![b"docs".to_vec(), b"old".to_vec()];
        let new_path = vec![b"docs".to_vec(), b"new".to_vec()];

        cache.update_stat(&old_path, Stat::new("old", Qid::file(7), 0o444));
        cache.update_stat(&new_path, Stat::new("new", Qid::file(8), 0o444));

        cache.mark_namespace_change("/docs/new", Some("/docs/old"));

        assert!(cache.stat_if_fresh(&old_path).is_none());
        assert!(cache.stat_if_fresh(&new_path).is_none());
    }
}
