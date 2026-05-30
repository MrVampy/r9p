use std::{
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    os::fd::IntoRawFd,
    os::unix::ffi::OsStrExt,
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct MountedNamespace {
    root: PathBuf,
}

impl MountedNamespace {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path(&self, namespace_path: impl AsRef<Path>) -> io::Result<PathBuf> {
        path_at(&self.root, namespace_path)
    }

    pub fn read_utf8(&self, namespace_path: impl AsRef<Path>) -> io::Result<String> {
        fs::read_to_string(self.path(namespace_path)?)
    }

    pub fn read_bytes_range(
        &self,
        namespace_path: impl AsRef<Path>,
        offset: u64,
        count: usize,
    ) -> io::Result<Vec<u8>> {
        let mut file = fs::File::open(self.path(namespace_path)?)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut buffer = vec![0_u8; count];
        let read = file.read(&mut buffer)?;
        buffer.truncate(read);
        Ok(buffer)
    }

    pub fn write_utf8(
        &self,
        namespace_path: impl AsRef<Path>,
        content: impl AsRef<str>,
    ) -> io::Result<()> {
        self.write_bytes(namespace_path, content.as_ref().as_bytes())
    }

    pub fn write_bytes(
        &self,
        namespace_path: impl AsRef<Path>,
        content: impl AsRef<[u8]>,
    ) -> io::Result<()> {
        let path = self.path(namespace_path)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        file.write_all(content.as_ref())?;
        close_file(file)
    }

    pub fn write_bytes_range(
        &self,
        namespace_path: impl AsRef<Path>,
        offset: u64,
        content: impl AsRef<[u8]>,
    ) -> io::Result<u64> {
        let path = self.path(namespace_path)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(content.as_ref())?;
        close_file(file)?;
        u64::try_from(content.as_ref().len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "write byte count overflow"))
    }

    pub fn read_directory(&self, namespace_path: impl AsRef<Path>) -> io::Result<Vec<String>> {
        let mut entries = fs::read_dir(self.path(namespace_path)?)?
            .map(|entry| {
                entry.and_then(|entry| {
                    entry
                        .file_name()
                        .into_string()
                        .map_err(|_| invalid_path("directory entry is not valid UTF-8"))
                })
            })
            .collect::<io::Result<Vec<_>>>()?;
        entries.sort();
        Ok(entries)
    }

    pub fn create_directory_all(&self, namespace_path: impl AsRef<Path>) -> io::Result<()> {
        fs::create_dir_all(self.path(namespace_path)?)
    }

    pub fn file_size(&self, namespace_path: impl AsRef<Path>) -> io::Result<u64> {
        Ok(fs::metadata(self.path(namespace_path)?)?.len())
    }

    pub fn truncate_file(&self, namespace_path: impl AsRef<Path>, length: u64) -> io::Result<()> {
        let file = OpenOptions::new()
            .write(true)
            .open(self.path(namespace_path)?)?;
        file.set_len(length)?;
        close_file(file)
    }

    pub fn rename_path(
        &self,
        source: impl AsRef<Path>,
        target: impl AsRef<Path>,
    ) -> io::Result<()> {
        let source = self.path(source)?;
        let target = self.path(target)?;
        fs::rename(source, target)
    }

    pub fn delete_file(&self, namespace_path: impl AsRef<Path>) -> io::Result<()> {
        fs::remove_file(self.path(namespace_path)?)
    }

    pub fn delete_tree(&self, namespace_path: impl AsRef<Path>) -> io::Result<()> {
        let path = self.path(namespace_path)?;
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        }
    }

    pub fn delete_directory(&self, namespace_path: impl AsRef<Path>) -> io::Result<()> {
        fs::remove_dir(self.path(namespace_path)?)
    }

    pub fn is_directory(&self, namespace_path: impl AsRef<Path>) -> io::Result<bool> {
        Ok(fs::metadata(self.path(namespace_path)?)?.is_dir())
    }
}

pub fn path_at(root: impl AsRef<Path>, namespace_path: impl AsRef<Path>) -> io::Result<PathBuf> {
    let mut path = root.as_ref().to_path_buf();
    for component in namespace_path.as_ref().components() {
        match component {
            Component::Prefix(_) => return Err(invalid_path("namespace path has a prefix")),
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => return Err(invalid_path("namespace path escapes root")),
            Component::Normal(segment) => push_component(&mut path, segment)?,
        }
    }
    Ok(path)
}

pub fn absolute_path_at(
    root: impl AsRef<Path>,
    namespace_path: impl AsRef<Path>,
) -> io::Result<PathBuf> {
    let namespace_path = namespace_path.as_ref();
    if !namespace_path.is_absolute() {
        return Err(invalid_path("namespace path is not absolute"));
    }
    path_at(root, namespace_path)
}

fn push_component(path: &mut PathBuf, segment: &OsStr) -> io::Result<()> {
    if segment.is_empty() {
        return Err(invalid_path("empty namespace path segment"));
    }
    if segment.as_bytes().contains(&0) {
        return Err(invalid_path("namespace path segment contains NUL"));
    }
    path.push(segment);
    Ok(())
}

fn invalid_path(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn close_file(file: fs::File) -> io::Result<()> {
    let fd = file.into_raw_fd();
    let status = unsafe { libc::close(fd) };
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        env,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn resolves_absolute_namespace_paths_under_root() -> io::Result<()> {
        let root = PathBuf::from("/tmp/r9p-mounted-root");
        assert_eq!(
            path_at(&root, "/runtime/status")?,
            root.join("runtime/status")
        );
        assert_eq!(
            path_at(&root, "runtime/status")?,
            root.join("runtime/status")
        );
        assert_eq!(path_at(&root, "/")?, root);
        Ok(())
    }

    #[test]
    fn rejects_parent_traversal() {
        let root = PathBuf::from("/tmp/r9p-mounted-root");
        let error = path_at(&root, "/runtime/../secret").expect_err("path should be rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn absolute_path_at_rejects_relative_paths() {
        let root = PathBuf::from("/tmp/r9p-mounted-root");
        let error = absolute_path_at(&root, "runtime/status").expect_err("path should be rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn rejects_nul_path_segments() {
        let root = PathBuf::from("/tmp/r9p-mounted-root");
        let error = path_at(&root, "/runtime/bad\0name").expect_err("path should be rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn reads_writes_ranges_and_metadata_under_mount_root() -> io::Result<()> {
        let root = fixture_root("mounted")?;
        let mounted = MountedNamespace::new(&root);

        mounted.create_directory_all("/runtime")?;
        mounted.write_utf8("/runtime/status", "abcdef")?;
        assert_eq!(mounted.read_utf8("runtime/status")?, "abcdef");
        assert_eq!(mounted.read_bytes_range("/runtime/status", 2, 3)?, b"cde");

        assert_eq!(mounted.write_bytes_range("/runtime/status", 3, b"XYZ")?, 3);
        assert_eq!(mounted.read_utf8("/runtime/status")?, "abcXYZ");
        assert_eq!(mounted.file_size("/runtime/status")?, 6);

        mounted.truncate_file("/runtime/status", 3)?;
        assert_eq!(mounted.read_utf8("/runtime/status")?, "abc");

        mounted.rename_path("/runtime/status", "/runtime/ready")?;
        assert_eq!(
            mounted.read_directory("/runtime")?,
            vec!["ready".to_string()]
        );
        mounted.delete_tree("/runtime/ready")?;
        mounted.write_utf8("/runtime/delete-file", "gone")?;
        mounted.delete_file("/runtime/delete-file")?;
        mounted.create_directory_all("/runtime/empty")?;
        mounted.delete_directory("/runtime/empty")?;
        mounted.delete_tree("/runtime")?;

        let _ = fs::remove_dir_all(root);
        Ok(())
    }

    #[test]
    fn delete_directory_is_not_recursive() -> io::Result<()> {
        let root = fixture_root("mounted-delete-directory")?;
        let mounted = MountedNamespace::new(&root);

        mounted.create_directory_all("/runtime/non-empty")?;
        mounted.write_utf8("/runtime/non-empty/file", "content")?;
        let error = mounted
            .delete_directory("/runtime/non-empty")
            .expect_err("direct directory delete should not recurse");
        assert_eq!(error.kind(), io::ErrorKind::DirectoryNotEmpty);
        assert!(root.join("runtime/non-empty/file").exists());

        let _ = fs::remove_dir_all(root);
        Ok(())
    }

    #[test]
    fn write_does_not_create_missing_parent_directories() -> io::Result<()> {
        let root = fixture_root("mounted-missing-parent")?;
        let mounted = MountedNamespace::new(&root);

        let error = mounted
            .write_utf8("/runtime/status", "abcdef")
            .expect_err("write should require an existing parent directory");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(!root.join("runtime").exists());

        let _ = fs::remove_dir_all(root);
        Ok(())
    }

    fn fixture_root(label: &str) -> io::Result<PathBuf> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = env::temp_dir().join(format!("r9p-fs-{label}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&path)?;
        Ok(path)
    }
}
