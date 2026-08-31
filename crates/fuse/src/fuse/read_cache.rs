use super::config::PERSISTENT_READ_CACHE_CHUNK_BYTES;
use crate::error::{Error, Result};
use r9p::stat::Stat;
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, FileExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_CACHE_CHUNKS: usize = 65_536;
const VOLUME_FILE: &str = "volume";
const LOCK_FILE: &str = "active.lock";
const CHUNKS_DIRECTORY: &str = "chunks";
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub(super) struct ReadCache {
    inner: Arc<Inner>,
}

struct Inner {
    chunks: PathBuf,
    max_bytes: u64,
    chunk_bytes: u32,
    state: Mutex<State>,
    completed: Condvar,
    storage: Mutex<()>,
    lock: File,
}

#[derive(Default)]
struct State {
    in_flight: BTreeSet<ChunkKey>,
    snapshot: CacheSnapshot,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct CacheSnapshot {
    pub(super) chunk_bytes: u32,
    pub(super) max_bytes: u64,
    pub(super) current_bytes: u64,
    pub(super) hit_chunks: u64,
    pub(super) miss_chunks: u64,
    pub(super) hit_bytes: u64,
    pub(super) fetched_bytes: u64,
    pub(super) evictions: u64,
    pub(super) read_errors: u64,
    pub(super) write_errors: u64,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct CacheIdentity {
    qtype: u8,
    qid_path: u64,
    qid_version: u32,
    length: u64,
    mtime: u32,
}

impl CacheIdentity {
    pub(super) fn from_stat(stat: &Stat) -> Option<Self> {
        if stat.qid.version == 0 || stat.length == 0 || stat.qid.is_dir() || stat.qid.is_symlink() {
            return None;
        }
        Some(Self {
            qtype: stat.qid.qtype,
            qid_path: stat.qid.path,
            qid_version: stat.qid.version,
            length: stat.length,
            mtime: stat.mtime,
        })
    }

    fn directory_name(self) -> String {
        format!(
            "q{:02x}-{:016x}-{:08x}-{:016x}-{:08x}",
            self.qtype, self.qid_path, self.qid_version, self.length, self.mtime
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ChunkKey {
    identity: CacheIdentity,
    index: u64,
}

struct CacheEntry {
    path: PathBuf,
    bytes: u64,
    modified: SystemTime,
}

impl ReadCache {
    pub(super) fn open(root: &Path, max_bytes: u64, volume_identity: &[u8]) -> Result<Self> {
        Self::open_with_chunk_bytes(
            root,
            max_bytes,
            volume_identity,
            PERSISTENT_READ_CACHE_CHUNK_BYTES,
        )
    }

    fn open_with_chunk_bytes(
        root: &Path,
        max_bytes: u64,
        volume_identity: &[u8],
        chunk_bytes: u32,
    ) -> Result<Self> {
        if !root.is_absolute()
            || max_bytes < u64::from(chunk_bytes)
            || max_bytes > u64::from(chunk_bytes).saturating_mul(MAX_CACHE_CHUNKS as u64)
            || chunk_bytes == 0
        {
            return Err(Error::new(libc::EINVAL, "read cache configuration invalid"));
        }
        ensure_private_directory(root)?;
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(root.join(LOCK_FILE))
            .map_err(|error| Error::io("open read cache lock", error))?;
        let lock_metadata = lock
            .metadata()
            .map_err(|error| Error::io("inspect read cache lock", error))?;
        if !private_owner(&lock_metadata) {
            return Err(Error::new(libc::EACCES, "read cache lock is not private"));
        }
        let chunks = {
            let _disk = CacheFileLock::acquire(&lock)?;
            bind_volume(root, &volume_binding(volume_identity, chunk_bytes)?)?;
            let chunks = root.join(CHUNKS_DIRECTORY);
            ensure_private_directory(&chunks)?;
            chunks
        };
        let cache = Self {
            inner: Arc::new(Inner {
                chunks,
                max_bytes,
                chunk_bytes,
                state: Mutex::new(State {
                    in_flight: BTreeSet::new(),
                    snapshot: CacheSnapshot {
                        chunk_bytes,
                        max_bytes,
                        ..CacheSnapshot::default()
                    },
                }),
                completed: Condvar::new(),
                storage: Mutex::new(()),
                lock,
            }),
        };
        cache.reconcile_usage()?;
        Ok(cache)
    }

    pub(super) fn snapshot(&self) -> CacheSnapshot {
        self.inner
            .state
            .lock()
            .map(|state| state.snapshot)
            .unwrap_or_default()
    }

    pub(super) fn read<F>(
        &self,
        identity: CacheIdentity,
        offset: u64,
        count: u32,
        mut fetch: F,
    ) -> Result<Vec<u8>>
    where
        F: FnMut(u64, u32) -> Result<Vec<u8>>,
    {
        let end = offset
            .checked_add(u64::from(count))
            .ok_or_else(|| Error::new(libc::EOVERFLOW, "read cache request overflow"))?
            .min(identity.length);
        let mut position = offset.min(end);
        let mut output = Vec::with_capacity(usize::try_from(end - position).unwrap_or(0));
        while position < end {
            let index = position / u64::from(self.inner.chunk_bytes);
            let chunk_start = index * u64::from(self.inner.chunk_bytes);
            let expected = identity
                .length
                .saturating_sub(chunk_start)
                .min(u64::from(self.inner.chunk_bytes));
            let expected = u32::try_from(expected)
                .map_err(|_| Error::new(libc::EOVERFLOW, "read cache chunk overflow"))?;
            let start = usize::try_from(position - chunk_start)
                .map_err(|_| Error::new(libc::EOVERFLOW, "read cache slice overflow"))?;
            let available = u64::from(expected).saturating_sub(position - chunk_start);
            let take = usize::try_from((end - position).min(available))
                .map_err(|_| Error::new(libc::EOVERFLOW, "read cache slice overflow"))?;
            let key = ChunkKey { identity, index };
            output.extend(self.chunk(key, expected, start, take, &mut fetch)?);
            position = position.saturating_add(u64::try_from(take).unwrap_or(0));
        }
        Ok(output)
    }

    fn chunk<F>(
        &self,
        key: ChunkKey,
        expected: u32,
        start: usize,
        take: usize,
        fetch: &mut F,
    ) -> Result<Vec<u8>>
    where
        F: FnMut(u64, u32) -> Result<Vec<u8>>,
    {
        loop {
            if let Some(bytes) = self.load_chunk(key, expected, start, take)? {
                self.note_hit(bytes.len());
                return Ok(bytes);
            }
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| Error::new(libc::EIO, "read cache state lock poisoned"))?;
            if state.in_flight.insert(key) {
                state.snapshot.miss_chunks = state.snapshot.miss_chunks.saturating_add(1);
                break;
            }
            state = self
                .inner
                .completed
                .wait(state)
                .map_err(|_| Error::new(libc::EIO, "read cache wait poisoned"))?;
            drop(state);
        }

        let chunk_start = key.index * u64::from(self.inner.chunk_bytes);
        let fetched = fetch(chunk_start, expected).and_then(|bytes| {
            if bytes.len() != usize::try_from(expected).unwrap_or(usize::MAX) {
                return Err(Error::new(libc::EIO, "read cache source range incomplete"));
            }
            if self.store_chunk(key, &bytes).is_err() {
                self.note_write_error();
            }
            let finish = start
                .checked_add(take)
                .ok_or_else(|| Error::new(libc::EOVERFLOW, "read cache slice overflow"))?;
            bytes
                .get(start..finish)
                .map(<[u8]>::to_vec)
                .ok_or_else(|| Error::new(libc::EIO, "read cache chunk incomplete"))
        });
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| Error::new(libc::EIO, "read cache state lock poisoned"))?;
        state.in_flight.remove(&key);
        if fetched.is_ok() {
            state.snapshot.fetched_bytes = state
                .snapshot
                .fetched_bytes
                .saturating_add(u64::from(expected));
        }
        self.inner.completed.notify_all();
        drop(state);
        fetched
    }

    fn load_chunk(
        &self,
        key: ChunkKey,
        expected: u32,
        start: usize,
        take: usize,
    ) -> Result<Option<Vec<u8>>> {
        let path = self.chunk_path(key);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => {
                self.note_read_error();
                return Ok(None);
            }
        };
        if !metadata.file_type().is_file()
            || !private_owner(&metadata)
            || metadata.len() != u64::from(expected)
        {
            self.discard_unusable_chunk(&path, &metadata);
            self.note_read_error();
            return Ok(None);
        }
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
        {
            Ok(file) => file,
            Err(_) => {
                self.discard_unusable_chunk(&path, &metadata);
                self.note_read_error();
                return Ok(None);
            }
        };
        let mut bytes = vec![0_u8; take];
        if file
            .read_exact_at(&mut bytes, u64::try_from(start).unwrap_or(u64::MAX))
            .is_err()
        {
            self.discard_unusable_chunk(&path, &metadata);
            self.note_read_error();
            return Ok(None);
        }
        let _ = file.set_modified(SystemTime::now());
        Ok(Some(bytes))
    }

    fn discard_unusable_chunk(&self, path: &Path, observed: &fs::Metadata) {
        if fs::symlink_metadata(path)
            .is_ok_and(|current| current.dev() == observed.dev() && current.ino() == observed.ino())
        {
            let _ = fs::remove_file(path);
        }
        let _ = self.reconcile_usage();
    }

    fn store_chunk(&self, key: ChunkKey, bytes: &[u8]) -> Result<()> {
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > self.inner.max_bytes {
            return Ok(());
        }
        let _storage = self.lock_storage()?;
        self.refresh_usage_from_disk()?;
        let destination = self.chunk_path(key);
        if destination.is_file() {
            return Ok(());
        }
        self.evict_for(u64::try_from(bytes.len()).unwrap_or(u64::MAX))?;
        let directory = destination
            .parent()
            .ok_or_else(|| Error::new(libc::EINVAL, "read cache chunk parent missing"))?;
        ensure_private_directory(directory)?;
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = directory.join(format!(".tmp-{}-{sequence}", std::process::id()));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|error| Error::io("create read cache chunk", error))?;
        let result = file
            .write_all(bytes)
            .and_then(|()| file.sync_data())
            .and_then(|()| fs::rename(&temporary, &destination));
        if let Err(error) = result {
            let _ = fs::remove_file(temporary);
            return Err(Error::io("publish read cache chunk", error));
        }
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| Error::new(libc::EIO, "read cache state lock poisoned"))?;
        state.snapshot.current_bytes = state
            .snapshot
            .current_bytes
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        Ok(())
    }

    fn reconcile_usage(&self) -> Result<()> {
        let _storage = self.lock_storage()?;
        self.refresh_usage_from_disk()?;
        self.evict_for(0)
    }

    fn refresh_usage_from_disk(&self) -> Result<()> {
        let entries = self.entries()?;
        let total = entries
            .iter()
            .fold(0_u64, |sum, entry| sum.saturating_add(entry.bytes));
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| Error::new(libc::EIO, "read cache state lock poisoned"))?;
        state.snapshot.current_bytes = total;
        Ok(())
    }

    fn lock_storage(&self) -> Result<CacheStorageLock<'_>> {
        let thread = self
            .inner
            .storage
            .lock()
            .map_err(|_| Error::new(libc::EIO, "read cache storage lock poisoned"))?;
        let file = CacheFileLock::acquire(&self.inner.lock)?;
        Ok(CacheStorageLock {
            _thread: thread,
            _file: file,
        })
    }

    fn evict_for(&self, incoming: u64) -> Result<()> {
        let current = self
            .inner
            .state
            .lock()
            .map_err(|_| Error::new(libc::EIO, "read cache state lock poisoned"))?
            .snapshot
            .current_bytes;
        if current.saturating_add(incoming) <= self.inner.max_bytes {
            return Ok(());
        }
        let mut entries = self.entries()?;
        entries.sort_by_key(|entry| entry.modified);
        let mut removed_bytes = 0_u64;
        let mut evictions = 0_u64;
        for entry in entries {
            if current
                .saturating_sub(removed_bytes)
                .saturating_add(incoming)
                <= self.inner.max_bytes
            {
                break;
            }
            if fs::remove_file(&entry.path).is_ok() {
                removed_bytes = removed_bytes.saturating_add(entry.bytes);
                evictions = evictions.saturating_add(1);
            }
        }
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| Error::new(libc::EIO, "read cache state lock poisoned"))?;
        state.snapshot.current_bytes = current.saturating_sub(removed_bytes);
        state.snapshot.evictions = state.snapshot.evictions.saturating_add(evictions);
        if state.snapshot.current_bytes.saturating_add(incoming) > self.inner.max_bytes {
            return Err(Error::new(
                libc::ENOSPC,
                "read cache quota cannot admit chunk",
            ));
        }
        Ok(())
    }

    fn entries(&self) -> Result<Vec<CacheEntry>> {
        let mut entries = Vec::new();
        for directory in
            fs::read_dir(&self.inner.chunks).map_err(|error| Error::io("scan read cache", error))?
        {
            let directory = directory.map_err(|error| Error::io("scan read cache", error))?;
            let directory_metadata = fs::symlink_metadata(directory.path())
                .map_err(|error| Error::io("inspect read cache entry", error))?;
            if !directory_metadata.file_type().is_dir() {
                continue;
            }
            if !private_owner(&directory_metadata) {
                return Err(Error::new(
                    libc::EACCES,
                    "read cache identity directory is not private",
                ));
            }
            for entry in fs::read_dir(directory.path())
                .map_err(|error| Error::io("scan read cache identity", error))?
            {
                let entry = entry.map_err(|error| Error::io("scan read cache identity", error))?;
                let metadata = fs::symlink_metadata(entry.path())
                    .map_err(|error| Error::io("inspect read cache chunk", error))?;
                if metadata.file_type().is_file()
                    && entry.file_name().to_string_lossy().starts_with(".tmp-")
                {
                    let _ = fs::remove_file(entry.path());
                    continue;
                }
                if !metadata.file_type().is_file()
                    || !private_owner(&metadata)
                    || !entry.file_name().to_string_lossy().ends_with(".chunk")
                {
                    continue;
                }
                if entries.len() >= MAX_CACHE_CHUNKS {
                    return Err(Error::new(
                        libc::E2BIG,
                        "read cache chunk count exceeds bound",
                    ));
                }
                entries.push(CacheEntry {
                    path: entry.path(),
                    bytes: metadata.len(),
                    modified: metadata.modified().unwrap_or(UNIX_EPOCH),
                });
            }
        }
        Ok(entries)
    }

    fn chunk_path(&self, key: ChunkKey) -> PathBuf {
        self.inner
            .chunks
            .join(key.identity.directory_name())
            .join(format!("{:016x}.chunk", key.index))
    }

    fn note_hit(&self, bytes: usize) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.snapshot.hit_chunks = state.snapshot.hit_chunks.saturating_add(1);
            state.snapshot.hit_bytes = state
                .snapshot
                .hit_bytes
                .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
        }
    }

    fn note_write_error(&self) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.snapshot.write_errors = state.snapshot.write_errors.saturating_add(1);
        }
    }

    fn note_read_error(&self) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.snapshot.read_errors = state.snapshot.read_errors.saturating_add(1);
        }
    }
}

struct CacheFileLock<'a> {
    file: &'a File,
}

impl<'a> CacheFileLock<'a> {
    fn acquire(file: &'a File) -> Result<Self> {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(Error::io(
                "lock read cache storage",
                std::io::Error::last_os_error(),
            ));
        }
        Ok(Self { file })
    }
}

impl Drop for CacheFileLock<'_> {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

struct CacheStorageLock<'a> {
    _thread: std::sync::MutexGuard<'a, ()>,
    _file: CacheFileLock<'a>,
}

fn ensure_private_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_dir() || !private_owner(&metadata) {
                return Err(Error::new(
                    libc::EACCES,
                    "read cache directory is not private",
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true).mode(0o700);
            builder
                .create(path)
                .map_err(|error| Error::io("create read cache directory", error))?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .map_err(|error| Error::io("secure read cache directory", error))?;
        }
        Err(error) => return Err(Error::io("inspect read cache directory", error)),
    }
    Ok(())
}

fn bind_volume(root: &Path, identity: &[u8]) -> Result<()> {
    let path = root.join(VOLUME_FILE);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() || !private_owner(&metadata) {
                return Err(Error::new(
                    libc::EACCES,
                    "read cache volume file is not private",
                ));
            }
            let mut file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&path)
                .map_err(|error| Error::io("open read cache volume", error))?;
            let mut existing = Vec::new();
            file.read_to_end(&mut existing)
                .map_err(|error| Error::io("read cache volume", error))?;
            if existing == identity {
                Ok(())
            } else {
                Err(Error::new(
                    libc::EINVAL,
                    "read cache volume identity mismatch",
                ))
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(path)
                .map_err(|error| Error::io("create read cache volume", error))?;
            file.write_all(identity)
                .and_then(|()| file.sync_all())
                .map_err(|error| Error::io("write read cache volume", error))
        }
        Err(error) => Err(Error::io("inspect read cache volume", error)),
    }
}

fn volume_binding(identity: &[u8], chunk_bytes: u32) -> Result<Vec<u8>> {
    let identity_bytes = u64::try_from(identity.len())
        .map_err(|_| Error::new(libc::EOVERFLOW, "read cache volume identity too large"))?;
    let mut binding = b"r9p persistent read cache\0".to_vec();
    binding.extend_from_slice(&chunk_bytes.to_le_bytes());
    binding.extend_from_slice(&identity_bytes.to_le_bytes());
    binding.extend_from_slice(identity);
    Ok(binding)
}

fn private_owner(metadata: &fs::Metadata) -> bool {
    metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o077 == 0
}

#[cfg(test)]
mod tests;
