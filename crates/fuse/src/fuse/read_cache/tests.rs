use super::{CacheIdentity, ChunkKey, ReadCache};
use crate::error::Error;
use r9p::{qid::Qid, stat::Stat};
use std::{
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Barrier,
    },
    thread,
    time::Duration,
};

static DIRECTORY_SEQUENCE: AtomicUsize = AtomicUsize::new(1);

fn test_directory(name: &str) -> PathBuf {
    let sequence = DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "r9p-read-cache-{name}-{}-{sequence}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    path
}

fn identity(path: u64, version: u32, length: u64) -> CacheIdentity {
    CacheIdentity {
        qtype: 0,
        qid_path: path,
        qid_version: version,
        length,
        mtime: 7,
    }
}

#[test]
fn only_versioned_known_files_are_cacheable() {
    let mut stat = Stat::new("file", Qid::new(0, 1, 7), 0o444);
    stat.length = 16;
    assert!(CacheIdentity::from_stat(&stat).is_some());
    stat.qid.version = 0;
    assert!(CacheIdentity::from_stat(&stat).is_none());
    stat.qid.version = 1;
    stat.length = 0;
    assert!(CacheIdentity::from_stat(&stat).is_none());
}

#[test]
fn cached_ranges_survive_source_failure_and_process_reopen() {
    let directory = test_directory("reopen");
    let cache = ReadCache::open_with_chunk_bytes(&directory, 64, b"volume-a", 16).expect("cache");
    let fetches = AtomicUsize::new(0);
    let first = cache
        .read(identity(1, 1, 32), 4, 8, |offset, count| {
            fetches.fetch_add(1, Ordering::SeqCst);
            Ok((offset..offset + u64::from(count))
                .map(|value| value as u8)
                .collect())
        })
        .expect("first read");
    assert_eq!(first, (4_u8..12).collect::<Vec<_>>());
    drop(cache);

    let cache =
        ReadCache::open_with_chunk_bytes(&directory, 64, b"volume-a", 16).expect("reopen cache");
    let second = cache
        .read(identity(1, 1, 32), 4, 8, |_, _| {
            Err(Error::new(libc::ENOTCONN, "source unavailable"))
        })
        .expect("cached read");
    assert_eq!(second, first);
    assert_eq!(fetches.load(Ordering::SeqCst), 1);
    assert_eq!(cache.snapshot().hit_chunks, 1);
    drop(cache);
    fs::remove_dir_all(directory).expect("remove cache directory");
}

#[test]
fn reads_cross_range_boundaries_without_overrunning_either_chunk() {
    let directory = test_directory("boundary");
    let cache = ReadCache::open_with_chunk_bytes(&directory, 64, b"volume-a", 16).expect("cache");
    let bytes = cache
        .read(identity(1, 1, 32), 12, 8, |offset, count| {
            Ok((offset..offset + u64::from(count))
                .map(|value| value as u8)
                .collect())
        })
        .expect("cross-range read");

    assert_eq!(bytes, (12_u8..20).collect::<Vec<_>>());
    assert_eq!(cache.snapshot().miss_chunks, 2);
    drop(cache);
    fs::remove_dir_all(directory).expect("remove cache directory");
}

#[test]
fn a_new_qid_generation_never_reuses_old_bytes() {
    let directory = test_directory("generation");
    let cache = ReadCache::open_with_chunk_bytes(&directory, 64, b"volume-a", 16).expect("cache");
    let first = cache
        .read(identity(1, 1, 16), 0, 8, |_, count| {
            Ok(vec![1; usize::try_from(count).expect("count")])
        })
        .expect("first generation");
    let second = cache
        .read(identity(1, 2, 16), 0, 8, |_, count| {
            Ok(vec![2; usize::try_from(count).expect("count")])
        })
        .expect("second generation");

    assert_eq!(first, vec![1; 8]);
    assert_eq!(second, vec![2; 8]);
    assert_eq!(cache.snapshot().miss_chunks, 2);
    drop(cache);
    fs::remove_dir_all(directory).expect("remove cache directory");
}

#[test]
fn cache_publication_failure_does_not_fail_the_source_read() {
    let directory = test_directory("publication-failure");
    let cache = ReadCache::open_with_chunk_bytes(&directory, 64, b"volume-a", 16).expect("cache");
    let identity = identity(1, 1, 16);
    let destination = cache.chunk_path(ChunkKey { identity, index: 0 });
    fs::create_dir_all(&destination).expect("conflicting destination");
    let bytes = cache
        .read(identity, 0, 8, |_, count| {
            Ok(vec![7; usize::try_from(count).expect("count")])
        })
        .expect("source read");

    assert_eq!(bytes, vec![7; 8]);
    assert_eq!(cache.snapshot().write_errors, 1);
    drop(cache);
    fs::remove_dir_all(directory).expect("remove cache directory");
}

#[test]
fn concurrent_misses_share_one_fetch() {
    let directory = test_directory("concurrent");
    let cache = ReadCache::open_with_chunk_bytes(&directory, 64, b"volume-a", 16).expect("cache");
    let barrier = Arc::new(Barrier::new(3));
    let fetches = Arc::new(AtomicUsize::new(0));
    let mut readers = Vec::new();
    for _ in 0..2 {
        let cache = cache.clone();
        let barrier = Arc::clone(&barrier);
        let fetches = Arc::clone(&fetches);
        readers.push(thread::spawn(move || {
            barrier.wait();
            cache
                .read(identity(1, 1, 32), 0, 8, |offset, count| {
                    fetches.fetch_add(1, Ordering::SeqCst);
                    thread::sleep(Duration::from_millis(25));
                    Ok((offset..offset + u64::from(count))
                        .map(|value| value as u8)
                        .collect())
                })
                .expect("read")
        }));
    }
    barrier.wait();
    let first = readers.remove(0).join().expect("first reader");
    let second = readers.remove(0).join().expect("second reader");
    assert_eq!(first, second);
    assert_eq!(fetches.load(Ordering::SeqCst), 1);
    drop(cache);
    fs::remove_dir_all(directory).expect("remove cache directory");
}

#[test]
fn quota_evicts_the_oldest_complete_chunk() {
    let directory = test_directory("quota");
    let cache = ReadCache::open_with_chunk_bytes(&directory, 16, b"volume-a", 16).expect("cache");
    for path in [1, 2] {
        cache
            .read(identity(path, 1, 16), 0, 8, |offset, count| {
                Ok((offset..offset + u64::from(count))
                    .map(|value| (value + path) as u8)
                    .collect())
            })
            .expect("read");
    }
    assert_eq!(cache.snapshot().current_bytes, 16);
    assert_eq!(cache.snapshot().evictions, 1);
    drop(cache);
    fs::remove_dir_all(directory).expect("remove cache directory");
}

#[test]
fn discarding_a_truncated_chunk_releases_quota_and_repopulates_it() {
    let directory = test_directory("truncated");
    let cache = ReadCache::open_with_chunk_bytes(&directory, 16, b"volume-a", 16).expect("cache");
    let identity = identity(1, 1, 16);
    cache
        .read(identity, 0, 8, |_, count| {
            Ok(vec![1; usize::try_from(count).expect("count")])
        })
        .expect("initial read");
    let destination = cache.chunk_path(ChunkKey { identity, index: 0 });
    fs::write(&destination, vec![9; 8]).expect("truncate cached chunk");

    let repaired = cache
        .read(identity, 0, 8, |_, count| {
            Ok(vec![2; usize::try_from(count).expect("count")])
        })
        .expect("repair read");
    let offline = cache
        .read(identity, 0, 8, |_, _| {
            Err(Error::new(libc::ENOTCONN, "source unavailable"))
        })
        .expect("repopulated cache read");

    assert_eq!(repaired, vec![2; 8]);
    assert_eq!(offline, repaired);
    assert_eq!(cache.snapshot().current_bytes, 16);
    assert_eq!(cache.snapshot().read_errors, 1);
    assert_eq!(cache.snapshot().write_errors, 0);
    assert_eq!(cache.snapshot().evictions, 0);
    drop(cache);
    fs::remove_dir_all(directory).expect("remove cache directory");
}

#[test]
fn a_cache_volume_cannot_change_identity_or_gain_a_second_owner() {
    let directory = test_directory("identity");
    let cache = ReadCache::open_with_chunk_bytes(&directory, 64, b"volume-a", 16).expect("cache");
    assert!(ReadCache::open_with_chunk_bytes(&directory, 64, b"volume-a", 16).is_err());
    drop(cache);
    assert!(ReadCache::open_with_chunk_bytes(&directory, 64, b"volume-a", 8).is_err());
    assert!(ReadCache::open_with_chunk_bytes(&directory, 64, b"volume-b", 16).is_err());
    assert!(fs::read(directory.join("volume"))
        .expect("volume binding")
        .ends_with(b"volume-a"));
    fs::remove_dir_all(directory).expect("remove cache directory");
}
