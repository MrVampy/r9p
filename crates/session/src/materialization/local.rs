use std::{
    collections::BTreeMap,
    fs::{self, DirBuilder, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{Error, Result};

use super::MaterializationLimits;

static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EntryKind {
    Directory,
    File(u64),
}

pub(super) struct LocalMaterialization {
    root: PathBuf,
    tree: PathBuf,
}

impl LocalMaterialization {
    pub(super) fn prepare(root: &Path) -> Result<Self> {
        ensure_directory(root, 0o700)?;
        let tree = root.join("tree");
        ensure_directory(&tree, 0o755)?;
        Ok(Self {
            root: root.to_path_buf(),
            tree,
        })
    }

    pub(super) fn tree(&self) -> &Path {
        &self.tree
    }

    pub(super) fn create_staging(&self) -> Result<PathBuf> {
        for _ in 0..32 {
            let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = self
                .root
                .join(format!(".snapshot-{}-{sequence}", std::process::id()));
            let mut builder = DirBuilder::new();
            builder.mode(0o700);
            match builder.create(&path) {
                Ok(()) => return Ok(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(Error::io("create materialization staging tree", error)),
            }
        }
        Err(Error::new(
            libc::EEXIST,
            "create materialization staging tree exhausted retries",
        ))
    }

    pub(super) fn write_staged_file(
        &self,
        staging: &Path,
        relative: &Path,
        bytes: &[u8],
    ) -> Result<()> {
        validate_relative(relative)?;
        let destination = staging.join(relative);
        let parent = destination
            .parent()
            .ok_or_else(|| Error::new(libc::EINVAL, "materialization path has no parent"))?;
        create_staged_directories(staging, parent)?;
        write_new_file(&destination, bytes)
    }

    pub(super) fn create_staged_directory(&self, staging: &Path, relative: &Path) -> Result<()> {
        validate_relative(relative)?;
        create_staged_directories(staging, &staging.join(relative))
    }

    pub(super) fn publish_snapshot(
        &self,
        staging: &Path,
        limits: &MaterializationLimits,
    ) -> Result<()> {
        require_direct_child(&self.root, staging)?;
        let staged = collect_entries(staging, limits.maximum_entries, limits.maximum_total_bytes)?;
        let live = collect_entries(
            &self.tree,
            super::MAXIMUM_ENTRIES,
            super::MAXIMUM_TOTAL_BYTES,
        )?;

        let mut directories = staged
            .iter()
            .filter_map(|(path, kind)| matches!(kind, EntryKind::Directory).then_some(path.clone()))
            .collect::<Vec<_>>();
        directories.sort_by_key(|path| path.components().count());
        for relative in directories {
            let destination = self.tree.join(&relative);
            if matches!(live.get(&relative), Some(EntryKind::File(_))) {
                remove_file(&destination)?;
            }
            ensure_directory(&destination, 0o755)?;
        }

        for (relative, kind) in &staged {
            if !matches!(kind, EntryKind::File(_)) {
                continue;
            }
            let source = staging.join(relative);
            let destination = self.tree.join(relative);
            if live.get(relative) == Some(&EntryKind::Directory) {
                remove_directory_tree(&destination)?;
            }
            fs::rename(&source, &destination)
                .map_err(|error| Error::io("publish materialization file", error))?;
        }

        let mut stale = live
            .iter()
            .filter_map(|(path, kind)| {
                (!staged.contains_key(path)).then_some((path.clone(), *kind))
            })
            .collect::<Vec<_>>();
        stale.sort_by_key(|(path, _)| std::cmp::Reverse(path.components().count()));
        for (relative, kind) in stale {
            let path = self.tree.join(relative);
            match kind {
                EntryKind::Directory => remove_empty_directory(&path)?,
                EntryKind::File(_) => remove_file(&path)?,
            }
        }

        fs::remove_dir_all(staging)
            .map_err(|error| Error::io("remove materialization staging tree", error))?;
        Ok(())
    }

    pub(super) fn replace_file(
        &self,
        relative: &Path,
        bytes: &[u8],
        limits: &MaterializationLimits,
    ) -> Result<()> {
        validate_relative(relative)?;
        let live = collect_entries(
            &self.tree,
            super::MAXIMUM_ENTRIES,
            super::MAXIMUM_TOTAL_BYTES,
        )?;
        if live.get(relative) == Some(&EntryKind::Directory) {
            return Err(Error::new(
                libc::EAGAIN,
                "file replaced a directory; materialization resync required",
            ));
        }
        let old_bytes = match live.get(relative) {
            Some(EntryKind::File(length)) => *length,
            _ => 0,
        };
        let new_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if new_bytes > limits.maximum_file_bytes {
            return Err(Error::new(
                libc::EFBIG,
                "materialization file exceeds its bound",
            ));
        }
        let current_bytes = live.values().fold(0_u64, |total, entry| {
            total.saturating_add(match entry {
                EntryKind::Directory => 0,
                EntryKind::File(length) => *length,
            })
        });
        let projected_bytes = current_bytes
            .saturating_sub(old_bytes)
            .saturating_add(new_bytes);
        if projected_bytes > limits.maximum_total_bytes {
            return Err(Error::new(
                libc::EFBIG,
                "materialization total byte bound exceeded",
            ));
        }

        let missing_parents = relative
            .parent()
            .into_iter()
            .flat_map(Path::ancestors)
            .take_while(|path| !path.as_os_str().is_empty())
            .filter(|path| !live.contains_key(*path))
            .count();
        let new_file = usize::from(!live.contains_key(relative));
        let projected_entries = live
            .len()
            .saturating_add(missing_parents)
            .saturating_add(new_file);
        if u64::try_from(projected_entries).unwrap_or(u64::MAX) > limits.maximum_entries {
            return Err(Error::new(
                libc::EFBIG,
                "materialization entry bound exceeded",
            ));
        }

        let destination = self.tree.join(relative);
        let parent = destination
            .parent()
            .ok_or_else(|| Error::new(libc::EINVAL, "materialization path has no parent"))?;
        create_live_directories(&self.tree, parent)?;
        if let Ok(metadata) = fs::symlink_metadata(&destination) {
            if metadata.file_type().is_symlink() {
                return Err(Error::new(libc::EPERM, "materialization rejects symlinks"));
            }
        }
        let temporary = temporary_sibling(&destination)?;
        let result = write_new_file(&temporary, bytes).and_then(|()| {
            fs::rename(&temporary, &destination)
                .map_err(|error| Error::io("publish materialization file", error))
        });
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    pub(super) fn remove(&self, relative: &Path) -> Result<()> {
        validate_relative(relative)?;
        let path = self.tree.join(relative);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(Error::io("inspect materialization path", error)),
        };
        if metadata.file_type().is_symlink() {
            return Err(Error::new(libc::EPERM, "materialization rejects symlinks"));
        }
        if metadata.is_dir() {
            remove_directory_tree(&path)
        } else if metadata.is_file() {
            remove_file(&path)
        } else {
            Err(Error::new(
                libc::EPERM,
                "materialization path is not a plain file or directory",
            ))
        }
    }
}

impl Drop for LocalMaterialization {
    fn drop(&mut self) {
        if let Ok(entries) = fs::read_dir(&self.root) {
            for entry in entries.flatten() {
                if entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with(".snapshot-"))
                {
                    let _ = fs::remove_dir_all(entry.path());
                }
            }
        }
    }
}

fn ensure_directory(path: &Path, mode: u32) -> Result<()> {
    let mut builder = DirBuilder::new();
    builder.mode(mode);
    match builder.create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(Error::io("create materialization directory", error)),
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| Error::io("inspect materialization directory", error))?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err(Error::new(
            libc::EPERM,
            "materialization directory is unsafe",
        ));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|error| Error::io("protect materialization directory", error))
}

fn create_staged_directories(staging: &Path, destination: &Path) -> Result<()> {
    require_descendant_or_same(staging, destination)?;
    create_directories_from(staging, destination, 0o755)
}

fn create_live_directories(tree: &Path, destination: &Path) -> Result<()> {
    require_descendant_or_same(tree, destination)?;
    create_directories_from(tree, destination, 0o755)
}

fn create_directories_from(root: &Path, destination: &Path, mode: u32) -> Result<()> {
    let relative = destination
        .strip_prefix(root)
        .map_err(|_| Error::new(libc::EINVAL, "materialization path escaped its root"))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(Error::new(libc::EINVAL, "materialization path is invalid"));
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(Error::new(
                        libc::EPERM,
                        "materialization directory path is unsafe",
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ensure_directory(&current, mode)?;
            }
            Err(error) => return Err(Error::io("inspect materialization path", error)),
        }
    }
    Ok(())
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| Error::io("create materialization file", error))?;
    file.write_all(bytes)
        .map_err(|error| Error::io("write materialization file", error))?;
    file.set_permissions(fs::Permissions::from_mode(0o444))
        .map_err(|error| Error::io("protect materialization file", error))
}

fn temporary_sibling(path: &Path) -> Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::new(libc::EINVAL, "materialization path has no parent"))?;
    for _ in 0..32 {
        let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(".cache-file-{}-{sequence}", std::process::id()));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(Error::new(
        libc::EEXIST,
        "create materialization temporary file exhausted retries",
    ))
}

fn collect_entries(
    root: &Path,
    maximum_entries: u64,
    maximum_total_bytes: u64,
) -> Result<BTreeMap<PathBuf, EntryKind>> {
    let mut entries = BTreeMap::new();
    let mut pending = vec![PathBuf::new()];
    let mut total_bytes = 0_u64;
    while let Some(relative) = pending.pop() {
        let directory = root.join(&relative);
        for entry in fs::read_dir(&directory)
            .map_err(|error| Error::io("read materialization directory", error))?
        {
            let entry =
                entry.map_err(|error| Error::io("read materialization directory entry", error))?;
            let mut child = relative.clone();
            child.push(entry.file_name());
            validate_relative(&child)?;
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|error| Error::io("inspect materialization path", error))?;
            let kind = if metadata.file_type().is_symlink() {
                return Err(Error::new(libc::EPERM, "materialization rejects symlinks"));
            } else if metadata.is_dir() {
                pending.push(child.clone());
                EntryKind::Directory
            } else if metadata.is_file() {
                total_bytes = total_bytes.saturating_add(metadata.len());
                if total_bytes > maximum_total_bytes {
                    return Err(Error::new(
                        libc::EFBIG,
                        "materialization total byte bound exceeded",
                    ));
                }
                EntryKind::File(metadata.len())
            } else {
                return Err(Error::new(
                    libc::EPERM,
                    "materialization path is not a plain file or directory",
                ));
            };
            if u64::try_from(entries.len()).unwrap_or(u64::MAX) >= maximum_entries {
                return Err(Error::new(
                    libc::EFBIG,
                    "materialization entry bound exceeded",
                ));
            }
            entries.insert(child, kind);
        }
    }
    Ok(entries)
}

fn validate_relative(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.as_os_str().as_encoded_bytes().len() > 4096
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(Error::new(libc::EINVAL, "materialization path is invalid"));
    }
    Ok(())
}

fn require_direct_child(parent: &Path, child: &Path) -> Result<()> {
    if child.parent() != Some(parent) {
        return Err(Error::new(
            libc::EINVAL,
            "materialization staging path escaped its root",
        ));
    }
    Ok(())
}

fn require_descendant_or_same(parent: &Path, child: &Path) -> Result<()> {
    if child == parent || child.starts_with(parent) {
        Ok(())
    } else {
        Err(Error::new(
            libc::EINVAL,
            "materialization path escaped its root",
        ))
    }
}

fn remove_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::io("remove materialization file", error)),
    }
}

fn remove_empty_directory(path: &Path) -> Result<()> {
    match fs::remove_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::io("remove materialization directory", error)),
    }
}

fn remove_directory_tree(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| Error::io("inspect materialization directory", error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Error::new(
            libc::EPERM,
            "materialization directory is unsafe",
        ));
    }
    fs::remove_dir_all(path).map_err(|error| Error::io("remove materialization directory", error))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> std::io::Result<Self> {
            let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "r9p-materialization-test-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path)?;
            Ok(Self(path))
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn limits() -> MaterializationLimits {
        MaterializationLimits {
            maximum_entries: 16,
            maximum_total_bytes: 1024,
            maximum_file_bytes: 512,
            maximum_depth: 4,
            parallelism: 2,
        }
    }

    #[test]
    fn snapshot_publish_replaces_and_removes_without_replacing_live_root() -> Result<()> {
        let temporary =
            TestDirectory::new().map_err(|error| Error::io("create test directory", error))?;
        let cache = LocalMaterialization::prepare(&temporary.0.join("memory"))?;
        let live_identity = fs::metadata(cache.tree())
            .map_err(|error| Error::io("inspect test tree", error))?
            .ino();
        cache.replace_file(Path::new("old.md"), b"old", &limits())?;

        let staging = cache.create_staging()?;
        cache.write_staged_file(&staging, Path::new("new.md"), b"new")?;
        cache.create_staged_directory(&staging, Path::new("nested"))?;
        cache.write_staged_file(&staging, Path::new("nested/value.md"), b"value")?;
        cache.publish_snapshot(&staging, &limits())?;

        assert_eq!(
            fs::metadata(cache.tree())
                .map_err(|error| Error::io("inspect test tree", error))?
                .ino(),
            live_identity
        );
        assert!(!cache.tree().join("old.md").exists());
        assert_eq!(
            fs::read(cache.tree().join("new.md"))
                .map_err(|error| Error::io("read test file", error))?,
            b"new"
        );
        assert_eq!(
            fs::read(cache.tree().join("nested/value.md"))
                .map_err(|error| Error::io("read test file", error))?,
            b"value"
        );
        Ok(())
    }

    #[test]
    fn incremental_updates_enforce_total_and_entry_bounds() -> Result<()> {
        let temporary =
            TestDirectory::new().map_err(|error| Error::io("create test directory", error))?;
        let cache = LocalMaterialization::prepare(&temporary.0.join("memory"))?;
        let mut configured = limits();
        configured.maximum_entries = 3;
        configured.maximum_total_bytes = 5;
        cache.replace_file(Path::new("one.md"), b"123", &configured)?;
        assert!(cache
            .replace_file(Path::new("two.md"), b"456", &configured)
            .is_err());
        cache.replace_file(Path::new("nested/two.md"), b"1", &configured)?;
        assert!(cache
            .replace_file(Path::new("three.md"), b"1", &configured)
            .is_err());
        Ok(())
    }

    #[test]
    fn relative_paths_cannot_escape_the_materialization() {
        assert!(validate_relative(Path::new("entry.md")).is_ok());
        assert!(validate_relative(Path::new("nested/entry.md")).is_ok());
        assert!(validate_relative(Path::new("../entry.md")).is_err());
        assert!(validate_relative(Path::new("/entry.md")).is_err());
    }
}
