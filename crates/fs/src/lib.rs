use r9p::{
    error::{Error, Result, ENOENT, ENOTDIR, EPERM},
    fid::Fid,
    qid::{Qid, DMDIR},
    server::{FileTree, OpenFile, ReadData},
    stat::Stat,
    OTRUNC, OWRITE,
};
use std::{
    collections::BTreeMap,
    os::fd::{AsRawFd, OwnedFd},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

mod unix_io;

use unix_io::{
    create_file_fd, duplicate_fd, fstat, is_read_only_mode, is_symlink, mkdir_child, node_from_fd,
    open_child, open_file_fd, open_read_fd, open_root, pread_file, pwrite_file, read_dir,
    read_link, remove_path, rename_path, stat_from_libc, truncate_fd, validate_rename_name,
    validate_truncate_length, Node,
};

#[derive(Clone)]
pub struct LocalTree {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LocalTreeConfig {
    pub writable: bool,
}

struct Inner {
    root: PathBuf,
    writable: bool,
    fids: BTreeMap<Fid, Node>,
    open_files: BTreeMap<Fid, OwnedFd>,
    stats: BTreeMap<u64, Stat>,
}

impl LocalTree {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_config(root, LocalTreeConfig::default())
    }

    pub fn open_with_config(root: impl AsRef<Path>, config: LocalTreeConfig) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let root_fd = open_root(&root)?;
        let root_node = node_from_fd(root_fd, b".".to_vec())?;
        if !root_node.stat.qid.is_dir() {
            return Err(Error::from_static(ENOTDIR));
        }

        let mut stats = BTreeMap::new();
        stats.insert(root_node.stat.qid.path, root_node.stat.clone());

        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                root,
                writable: config.writable,
                fids: BTreeMap::new(),
                open_files: BTreeMap::new(),
                stats,
            })),
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Inner>> {
        self.inner
            .lock()
            .map_err(|_| Error::from("local fs tree lock poisoned"))
    }
}

impl FileTree for LocalTree {
    fn reset(&mut self) -> Result<()> {
        let mut inner = self.lock()?;
        inner.fids.clear();
        inner.open_files.clear();
        inner.stats.clear();
        Ok(())
    }

    fn attach(&mut self, fid: Fid, _uname: &[u8], _aname: &[u8]) -> Result<Qid> {
        let root = self.lock()?.root.clone();
        let root_fd = open_root(&root)?;
        let node = node_from_fd(root_fd, b".".to_vec())?;
        let qid = node.stat.qid;
        let mut inner = self.lock()?;
        inner.remember(&node.stat);
        inner.open_files.remove(&fid);
        inner.fids.insert(fid, node);
        Ok(qid)
    }

    fn walk(&mut self, fid: Fid, newfid: Fid, _start: Qid, names: &[Vec<u8>]) -> Result<Vec<Qid>> {
        let mut inner = self.lock()?;
        let mut current = inner
            .fids
            .get(&fid)
            .ok_or_else(|| Error::from_static(r9p::error::EBADFID))?
            .duplicate()?;
        let mut qids = Vec::with_capacity(names.len());

        for name in names {
            match open_child(current.fd.as_raw_fd(), name) {
                Ok(child) => {
                    qids.push(child.stat.qid);
                    inner.remember(&child.stat);
                    current = child;
                }
                // A short walk carries no reason. 9P2000 admits one only after
                // the first element; the first must answer Rerror.
                Err(error) if error.message() == ENOENT.as_bytes() && !qids.is_empty() => break,
                Err(error) => return Err(error),
            }
        }

        if qids.len() == names.len() {
            inner.open_files.remove(&newfid);
            inner.fids.insert(newfid, current);
        }

        Ok(qids)
    }

    fn open(&mut self, fid: Fid, qid: Qid, mode: u8) -> Result<OpenFile> {
        let mut inner = self.lock()?;
        if !inner.writable && !is_read_only_mode(mode) {
            return Err(Error::from_static(EPERM));
        }
        let (fd, name, is_dir, is_link) = {
            let node = inner
                .fids
                .get(&fid)
                .ok_or_else(|| Error::from_static(r9p::error::EBADFID))?;
            if node.stat.qid != qid {
                return Err(Error::from_static(r9p::error::EBADFID));
            }
            (
                node.fd.as_raw_fd(),
                node.stat.name.clone(),
                qid.is_dir(),
                is_symlink(&node.stat),
            )
        };
        if is_dir || is_link {
            if !is_read_only_mode(mode) {
                return Err(Error::from_static(EPERM));
            }
            return Ok(OpenFile { qid, iounit: 0 });
        }

        let file = open_file_fd(fd, mode)?;
        let refreshed = if mode & OTRUNC != 0 {
            Some(stat_from_libc(&fstat(fd)?, name))
        } else {
            None
        };
        inner.open_files.insert(fid, file);
        let mut opened_qid = qid;
        if let Some(stat) = refreshed {
            opened_qid = stat.qid;
            if let Some(node) = inner.fids.get_mut(&fid) {
                node.stat = stat.clone();
            }
            inner.remember(&stat);
        }
        Ok(OpenFile {
            qid: opened_qid,
            iounit: 0,
        })
    }

    fn read(&mut self, fid: Fid, qid: Qid, offset: u64, count: u32) -> Result<ReadData> {
        let inner = self.lock()?;
        let node = inner
            .fids
            .get(&fid)
            .ok_or_else(|| Error::from_static(r9p::error::EBADFID))?;
        if node.stat.qid != qid {
            return Err(Error::from_static(r9p::error::EBADFID));
        }

        if qid.is_dir() {
            return read_dir(node.fd.as_raw_fd()).map(ReadData::Directory);
        }
        if is_symlink(&node.stat) {
            return read_link(node.fd.as_raw_fd()).map(ReadData::Bytes);
        }

        let file = match inner.open_files.get(&fid) {
            Some(file) => duplicate_fd(file.as_raw_fd())?,
            None => open_read_fd(node.fd.as_raw_fd(), false)?,
        };
        pread_file(file.as_raw_fd(), offset, count).map(ReadData::Bytes)
    }

    fn stat(&mut self, qid: Qid) -> Result<Stat> {
        self.lock()?
            .stats
            .get(&qid.path)
            .cloned()
            .ok_or_else(|| Error::from_static(ENOENT))
    }

    fn clunk(&mut self, fid: Fid, _qid: Qid) -> Result<()> {
        let mut inner = self.lock()?;
        inner.open_files.remove(&fid);
        inner.fids.remove(&fid);
        Ok(())
    }

    fn create(&mut self, fid: Fid, qid: Qid, name: &[u8], perm: u32, mode: u8) -> Result<OpenFile> {
        let mut inner = self.lock()?;
        if !inner.writable {
            return Err(Error::from_static(EPERM));
        }
        let parent_fd = {
            let parent = inner
                .fids
                .get(&fid)
                .ok_or_else(|| Error::from_static(r9p::error::EBADFID))?;
            if parent.stat.qid != qid || !qid.is_dir() {
                return Err(Error::from_static(r9p::error::EBADFID));
            }
            parent.fd.as_raw_fd()
        };

        let node = if perm & DMDIR != 0 {
            mkdir_child(parent_fd, name, perm)?;
            inner.open_files.remove(&fid);
            open_child(parent_fd, name)?
        } else {
            let file = create_file_fd(parent_fd, name, perm, mode)?;
            let node = node_from_fd(duplicate_fd(file.as_raw_fd())?, name.to_vec())?;
            inner.open_files.insert(fid, file);
            node
        };
        let qid = node.stat.qid;
        inner.remember(&node.stat);
        inner.fids.insert(fid, node);
        Ok(OpenFile { qid, iounit: 0 })
    }

    fn write(&mut self, fid: Fid, qid: Qid, offset: u64, data: &[u8]) -> Result<u32> {
        let mut inner = self.lock()?;
        if !inner.writable {
            return Err(Error::from_static(EPERM));
        }
        let (fd, name) = {
            let node = inner
                .fids
                .get(&fid)
                .ok_or_else(|| Error::from_static(r9p::error::EBADFID))?;
            if node.stat.qid != qid || qid.is_dir() || is_symlink(&node.stat) {
                return Err(Error::from_static(r9p::error::EBADFID));
            }
            (node.fd.as_raw_fd(), node.stat.name.clone())
        };
        let file = match inner.open_files.get(&fid) {
            Some(file) => duplicate_fd(file.as_raw_fd())?,
            None => open_file_fd(fd, OWRITE)?,
        };
        let written = pwrite_file(file.as_raw_fd(), offset, data)?;
        let stat = stat_from_libc(&fstat(fd)?, name);
        if let Some(node) = inner.fids.get_mut(&fid) {
            node.stat = stat.clone();
        }
        inner.remember(&stat);
        Ok(written)
    }

    fn remove(&mut self, fid: Fid, qid: Qid) -> Result<()> {
        let mut inner = self.lock()?;
        if !inner.writable {
            return Err(Error::from_static(EPERM));
        }
        let (fd, is_dir) = {
            let node = inner
                .fids
                .get(&fid)
                .ok_or_else(|| Error::from_static(r9p::error::EBADFID))?;
            if node.stat.qid != qid {
                return Err(Error::from_static(r9p::error::EBADFID));
            }
            (node.fd.as_raw_fd(), qid.is_dir())
        };
        remove_path(fd, is_dir)?;
        inner.open_files.remove(&fid);
        inner.fids.remove(&fid);
        inner.stats.remove(&qid.path);
        Ok(())
    }

    fn wstat(&mut self, fid: Fid, qid: Qid, stat: &Stat) -> Result<()> {
        let mut inner = self.lock()?;
        if !inner.writable {
            return Err(Error::from_static(EPERM));
        }
        let (fd, old_name, old_length, is_dir, is_link) = {
            let node = inner
                .fids
                .get(&fid)
                .ok_or_else(|| Error::from_static(r9p::error::EBADFID))?;
            if node.stat.qid != qid {
                return Err(Error::from_static(r9p::error::EBADFID));
            }
            (
                node.fd.as_raw_fd(),
                node.stat.name.clone(),
                node.stat.length,
                qid.is_dir(),
                is_symlink(&node.stat),
            )
        };

        if stat.mode != u32::MAX || stat.mtime != u32::MAX || !stat.gid.is_empty() {
            return Err(Error::from_static(EPERM));
        }

        let rename_requested = !stat.name.is_empty() && stat.name != old_name;
        let truncate_requested = stat.length != u64::MAX && stat.length != old_length;
        if rename_requested {
            validate_rename_name(&stat.name)?;
        }
        if truncate_requested {
            if is_dir || is_link {
                return Err(Error::from_static(EPERM));
            }
            validate_truncate_length(stat.length)?;
        }
        if rename_requested && truncate_requested {
            return Err(Error::from(
                "wstat -- local backend cannot atomically rename and truncate",
            ));
        }

        let mut name = old_name;
        if rename_requested {
            rename_path(fd, &stat.name)?;
            name = stat.name.clone();
        }
        if truncate_requested {
            truncate_fd(fd, stat.length)?;
        }

        let refreshed = stat_from_libc(&fstat(fd)?, name);
        if let Some(node) = inner.fids.get_mut(&fid) {
            node.stat = refreshed.clone();
        }
        inner.remember(&refreshed);
        Ok(())
    }
}

impl Inner {
    fn remember(&mut self, stat: &Stat) {
        self.stats.insert(stat.qid.path, stat.clone());
    }
}

#[cfg(test)]
mod tests;
