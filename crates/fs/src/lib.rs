use r9p::{
    blocking::{OEXEC, ORDWR, OREAD, OTRUNC, OWRITE},
    error::{Error, Result, EEXIST, ENOTDIR, EPERM},
    fid::Fid,
    qid::{Qid, DMDIR, DMSYMLINK, QTFILE, QTSYMLINK},
    server::{FileTree, OpenFile, ReadData, ServerCompletion, ServerRequestKind},
    stat::Stat,
};
use std::{
    collections::BTreeMap,
    ffi::{CStr, CString, OsStr},
    fs,
    mem::MaybeUninit,
    os::{
        fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd},
        unix::ffi::OsStrExt,
    },
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub mod mounted;

const ENOENT_PROTOCOL: &str = EEXIST;
const EMFILE_PROTOCOL: &str = "too many open files";

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

struct Node {
    fd: OwnedFd,
    stat: Stat,
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

    pub fn root(&self) -> Result<PathBuf> {
        Ok(self.lock()?.root.clone())
    }

    pub fn perform(&self, request: &ServerRequestKind) -> Result<ServerCompletion> {
        match request {
            ServerRequestKind::Auth { afid, uname, aname } => self
                .clone()
                .auth(*afid, uname, aname)
                .map(|qid| ServerCompletion::Auth { qid }),
            ServerRequestKind::Attach {
                fid,
                afid,
                uname,
                aname,
            } => {
                let qid = if *afid == r9p::NOFID {
                    self.clone().attach(*fid, uname, aname)?
                } else {
                    self.clone().attach_with_auth(*fid, *afid, uname, aname)?
                };
                Ok(ServerCompletion::Attach { qid })
            }
            ServerRequestKind::Walk {
                fid,
                newfid,
                start,
                wnames,
            } => self
                .clone()
                .walk(*fid, *newfid, *start, wnames)
                .map(|qids| ServerCompletion::Walk { qids }),
            ServerRequestKind::Open { fid, qid, mode } => self
                .clone()
                .open(*fid, *qid, *mode)
                .map(ServerCompletion::Open),
            ServerRequestKind::Create {
                fid,
                qid,
                name,
                perm,
                mode,
            } => self
                .clone()
                .create(*fid, *qid, name, *perm, *mode)
                .map(ServerCompletion::Create),
            ServerRequestKind::Read {
                fid,
                qid,
                offset,
                count,
            } => self
                .clone()
                .read(*fid, *qid, *offset, *count)
                .map(ServerCompletion::Read),
            ServerRequestKind::Write {
                fid,
                qid,
                offset,
                data,
            } => self
                .clone()
                .write(*fid, *qid, *offset, data)
                .map(|count| ServerCompletion::Write { count }),
            ServerRequestKind::Clunk { fid, qid } => self
                .clone()
                .clunk(*fid, *qid)
                .map(|()| ServerCompletion::Clunk),
            ServerRequestKind::Remove { fid, qid } => self
                .clone()
                .remove(*fid, *qid)
                .map(|()| ServerCompletion::Remove),
            ServerRequestKind::Stat { fid, qid } => {
                let _ = fid;
                self.clone()
                    .stat(*qid)
                    .map(|stat| ServerCompletion::Stat { stat })
            }
            ServerRequestKind::Wstat { fid, qid, stat } => self
                .clone()
                .wstat(*fid, *qid, stat)
                .map(|()| ServerCompletion::Wstat),
        }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Inner>> {
        self.inner
            .lock()
            .map_err(|_| Error::from("local fs tree lock poisoned"))
    }
}

impl FileTree for LocalTree {
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
                Err(error) if error.message() == ENOENT_PROTOCOL.as_bytes() => break,
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
        if let Some(stat) = refreshed {
            if let Some(node) = inner.fids.get_mut(&fid) {
                node.stat = stat.clone();
            }
            inner.remember(&stat);
        }
        Ok(OpenFile { qid, iounit: 0 })
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
            .ok_or_else(|| Error::from_static(ENOENT_PROTOCOL))
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
        let (fd, old_name, is_dir, is_link) = {
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

        let mut name = old_name;
        if !stat.name.is_empty() && stat.name != name {
            rename_path(fd, &stat.name)?;
            name = stat.name.clone();
        }
        if stat.length != u64::MAX && !is_dir && !is_link {
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

impl Node {
    fn duplicate(&self) -> Result<Self> {
        Ok(Self {
            fd: duplicate_fd(self.fd.as_raw_fd())?,
            stat: self.stat.clone(),
        })
    }
}

fn is_read_only_mode(mode: u8) -> bool {
    let access = mode & 0x03;
    let flags = mode & !0x03;
    matches!(access, OREAD | OEXEC) && flags == 0
}

fn validate_mutable_mode(mode: u8) -> Result<()> {
    let flags = mode & !(0x03 | OTRUNC);
    if flags == 0 {
        Ok(())
    } else {
        Err(Error::from_static(EPERM))
    }
}

fn libc_open_flags(mode: u8) -> Result<libc::c_int> {
    validate_mutable_mode(mode)?;
    let access = match mode & 0x03 {
        OREAD | OEXEC => libc::O_RDONLY,
        OWRITE => libc::O_WRONLY,
        ORDWR => libc::O_RDWR,
        _ => return Err(Error::from_static(EPERM)),
    };
    Ok(access | libc::O_CLOEXEC | if mode & OTRUNC != 0 { libc::O_TRUNC } else { 0 })
}

fn open_root(root: &Path) -> Result<OwnedFd> {
    let c_path = CString::new(root.as_os_str().as_bytes())
        .map_err(|_| Error::from("root path contains NUL byte"))?;
    let raw = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    owned_fd(raw).map_err(|error| map_io("open export root", error))
}

fn open_child(parent: RawFd, name: &[u8]) -> Result<Node> {
    if name == b"." || name == b".." {
        return Err(Error::from_static(ENOENT_PROTOCOL));
    }
    let c_name = CString::new(name).map_err(|_| Error::from_static(ENOENT_PROTOCOL))?;
    let raw = unsafe {
        libc::openat(
            parent,
            c_name.as_ptr(),
            libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    let fd = owned_fd(raw).map_err(|error| map_io("open child", error))?;
    node_from_fd(fd, name.to_vec())
}

fn mkdir_child(parent: RawFd, name: &[u8], perm: u32) -> Result<()> {
    let c_name = child_name(name)?;
    let status = unsafe { libc::mkdirat(parent, c_name.as_ptr(), (perm & 0o777) as libc::mode_t) };
    if status == 0 {
        Ok(())
    } else {
        Err(map_io("mkdirat", std::io::Error::last_os_error()))
    }
}

fn create_file_fd(parent: RawFd, name: &[u8], perm: u32, mode: u8) -> Result<OwnedFd> {
    let c_name = child_name(name)?;
    let flags = libc_open_flags(mode)? | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW;
    let raw = unsafe {
        libc::openat(
            parent,
            c_name.as_ptr(),
            flags,
            (perm & 0o777) as libc::mode_t,
        )
    };
    owned_fd(raw).map_err(|error| map_io("create file", error))
}

fn child_name(name: &[u8]) -> Result<CString> {
    if name.is_empty() || name == b"." || name == b".." || name.contains(&b'/') {
        return Err(Error::from_static(ENOENT_PROTOCOL));
    }
    CString::new(name).map_err(|_| Error::from_static(ENOENT_PROTOCOL))
}

fn node_from_fd(fd: OwnedFd, name: Vec<u8>) -> Result<Node> {
    let st = fstat(fd.as_raw_fd())?;
    let kind = st.st_mode & libc::S_IFMT;
    if kind != libc::S_IFREG && kind != libc::S_IFDIR && kind != libc::S_IFLNK {
        return Err(Error::from_static(ENOENT_PROTOCOL));
    }
    let stat = stat_from_libc(&st, name);
    Ok(Node { fd, stat })
}

fn stat_from_libc(st: &libc::stat, name: Vec<u8>) -> Stat {
    let kind = st.st_mode & libc::S_IFMT;
    let is_dir = kind == libc::S_IFDIR;
    let is_symlink = kind == libc::S_IFLNK;
    let qtype = if is_dir {
        r9p::qid::QTDIR
    } else if is_symlink {
        QTSYMLINK
    } else {
        QTFILE
    };
    let mut stat = Stat::new(
        name,
        Qid::new(qtype, st.st_mtime as u32, qid_path(st.st_dev, st.st_ino)),
        (st.st_mode & 0o777)
            | if is_dir { DMDIR } else { 0 }
            | if is_symlink { DMSYMLINK } else { 0 },
    );
    stat.atime = st.st_atime as u32;
    stat.mtime = st.st_mtime as u32;
    stat.length = if is_dir { 0 } else { st.st_size.max(0) as u64 };
    stat.uid = st.st_uid.to_string().into_bytes();
    stat.gid = st.st_gid.to_string().into_bytes();
    stat
}

fn is_symlink(stat: &Stat) -> bool {
    stat.qid.is_symlink() || stat.mode & DMSYMLINK != 0
}

fn qid_path(dev: u64, ino: u64) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in dev.to_le_bytes().into_iter().chain(ino.to_le_bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn read_dir(path_fd: RawFd) -> Result<Vec<Stat>> {
    let dir_fd = open_read_fd(path_fd, true)?;
    let raw = dir_fd.into_raw_fd();
    let dir = unsafe { libc::fdopendir(raw) };
    if dir.is_null() {
        let error = std::io::Error::last_os_error();
        let _ = unsafe { OwnedFd::from_raw_fd(raw) };
        return Err(map_io("fdopendir", error));
    }

    let mut stats = Vec::new();
    loop {
        let entry = unsafe { libc::readdir(dir) };
        if entry.is_null() {
            break;
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        if let Ok(node) = open_child(path_fd, name) {
            stats.push(node.stat);
        }
    }

    let close_status = unsafe { libc::closedir(dir) };
    if close_status != 0 {
        return Err(map_io("closedir", std::io::Error::last_os_error()));
    }
    stats.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(stats)
}

fn pread_file(fd: RawFd, offset: u64, count: u32) -> Result<Vec<u8>> {
    let len = usize::try_from(count).map_err(|_| Error::from("read count too large"))?;
    let mut buffer = vec![0_u8; len];
    let read = unsafe {
        libc::pread(
            fd,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            offset as libc::off_t,
        )
    };
    if read < 0 {
        return Err(map_io("pread", std::io::Error::last_os_error()));
    }
    let read = usize::try_from(read).map_err(|_| Error::from("read count overflow"))?;
    buffer.truncate(read);
    Ok(buffer)
}

fn read_link(fd: RawFd) -> Result<Vec<u8>> {
    let empty_path = CString::new("").map_err(|_| Error::from("empty path contains NUL"))?;
    let mut capacity = 256_usize;
    loop {
        let mut buffer = vec![0_u8; capacity];
        let read = unsafe {
            libc::readlinkat(
                fd,
                empty_path.as_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
            )
        };
        if read < 0 {
            return Err(map_io("readlinkat", std::io::Error::last_os_error()));
        }
        let read = usize::try_from(read).map_err(|_| Error::from("readlink size overflow"))?;
        if read < buffer.len() {
            buffer.truncate(read);
            return Ok(buffer);
        }
        capacity = capacity
            .checked_mul(2)
            .filter(|next| *next <= 1024 * 1024)
            .ok_or_else(|| Error::from("symlink target too large"))?;
    }
}

fn open_read_fd(path_fd: RawFd, directory: bool) -> Result<OwnedFd> {
    let proc_path = format!("/proc/self/fd/{path_fd}");
    let c_path = CString::new(proc_path).map_err(|_| Error::from("proc fd path contains NUL"))?;
    let mut flags = libc::O_RDONLY | libc::O_CLOEXEC;
    if directory {
        flags |= libc::O_DIRECTORY;
    }
    let raw = unsafe { libc::open(c_path.as_ptr(), flags) };
    owned_fd(raw).map_err(|error| map_io("open proc fd", error))
}

fn open_file_fd(path_fd: RawFd, mode: u8) -> Result<OwnedFd> {
    let proc_path = format!("/proc/self/fd/{path_fd}");
    let c_path = CString::new(proc_path).map_err(|_| Error::from("proc fd path contains NUL"))?;
    let raw = unsafe { libc::open(c_path.as_ptr(), libc_open_flags(mode)?) };
    owned_fd(raw).map_err(|error| map_io("open proc fd", error))
}

fn pwrite_file(fd: RawFd, offset: u64, data: &[u8]) -> Result<u32> {
    let written =
        unsafe { libc::pwrite(fd, data.as_ptr().cast(), data.len(), offset as libc::off_t) };
    if written < 0 {
        return Err(map_io("pwrite", std::io::Error::last_os_error()));
    }
    u32::try_from(written).map_err(|_| Error::from("write count overflow"))
}

fn truncate_fd(path_fd: RawFd, length: u64) -> Result<()> {
    let file = open_file_fd(path_fd, OWRITE)?;
    let status = unsafe { libc::ftruncate(file.as_raw_fd(), length as libc::off_t) };
    if status == 0 {
        Ok(())
    } else {
        Err(map_io("ftruncate", std::io::Error::last_os_error()))
    }
}

fn remove_path(path_fd: RawFd, is_dir: bool) -> Result<()> {
    let path = proc_fd_path(path_fd)?;
    let result = if is_dir {
        fs::remove_dir(&path)
    } else {
        fs::remove_file(&path)
    };
    result.map_err(|error| map_io("remove path", error))
}

fn rename_path(path_fd: RawFd, new_name: &[u8]) -> Result<()> {
    if new_name.is_empty() || new_name == b"." || new_name == b".." || new_name.contains(&b'/') {
        return Err(Error::from_static(ENOENT_PROTOCOL));
    }
    let source = proc_fd_path(path_fd)?;
    let parent = source
        .parent()
        .ok_or_else(|| Error::from_static(ENOENT_PROTOCOL))?;
    let target = parent.join(OsStr::from_bytes(new_name));
    fs::rename(&source, &target).map_err(|error| map_io("rename path", error))
}

fn proc_fd_path(path_fd: RawFd) -> Result<PathBuf> {
    fs::read_link(format!("/proc/self/fd/{path_fd}"))
        .map_err(|error| map_io("read proc fd link", error))
}

fn duplicate_fd(fd: RawFd) -> Result<OwnedFd> {
    let raw = unsafe { libc::dup(fd) };
    owned_fd(raw).map_err(|error| map_io("dup fd", error))
}

fn fstat(fd: RawFd) -> Result<libc::stat> {
    let mut st = MaybeUninit::<libc::stat>::uninit();
    let status = unsafe { libc::fstat(fd, st.as_mut_ptr()) };
    if status != 0 {
        return Err(map_io("fstat", std::io::Error::last_os_error()));
    }
    Ok(unsafe { st.assume_init() })
}

fn owned_fd(raw: RawFd) -> std::io::Result<OwnedFd> {
    if raw < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(raw) })
    }
}

fn map_io(context: &'static str, error: std::io::Error) -> Error {
    match error.raw_os_error() {
        Some(libc::ENOENT | libc::ENOTDIR | libc::ELOOP) => Error::from_static(ENOENT_PROTOCOL),
        Some(libc::EACCES | libc::EPERM) => Error::from_static(EPERM),
        Some(libc::EMFILE | libc::ENFILE) => Error::from_static(EMFILE_PROTOCOL),
        _ => Error::from(format!("{context}: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use r9p::{
        blocking::{OREAD, OTRUNC, OWRITE},
        message::{RMessage, TMessage},
        server::Server,
        stat::decode_dir_entries,
    };
    use std::{
        env, fs,
        os::unix::{ffi::OsStrExt, fs::symlink},
        process,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn serves_file_reads_by_offset() -> Result<()> {
        let root = fixture_root("read")?;
        fs::write(root.join("body"), b"abcdef").map_err(|error| Error::from(error.to_string()))?;

        let mut server = Server::new(LocalTree::open(&root)?);
        attach(&mut server);
        walk(&mut server, 1, 2, b"body");
        open(&mut server, 2);

        let reply = server.handle(TMessage::Read {
            tag: 4,
            fid: 2,
            offset: 2,
            count: 3,
        });
        assert_eq!(
            reply,
            RMessage::Read {
                tag: 4,
                data: b"cde".to_vec()
            }
        );

        remove_fixture(root);
        Ok(())
    }

    #[test]
    fn lists_regular_directory_entries() -> Result<()> {
        let root = fixture_root("dir")?;
        fs::write(root.join("a"), b"a").map_err(|error| Error::from(error.to_string()))?;
        fs::write(root.join("b"), b"b").map_err(|error| Error::from(error.to_string()))?;

        let mut server = Server::new(LocalTree::open(&root)?);
        attach(&mut server);
        clone_fid(&mut server, 1, 2);
        open(&mut server, 2);
        let reply = server.handle(TMessage::Read {
            tag: 3,
            fid: 2,
            offset: 0,
            count: 8192,
        });
        let data = match reply {
            RMessage::Read { data, .. } => data,
            other => return Err(Error::from(format!("unexpected reply: {other:?}"))),
        };
        let names = decode_dir_entries(&data)?
            .into_iter()
            .map(|stat| stat.name)
            .collect::<Vec<_>>();
        assert_eq!(names, [b"a".to_vec(), b"b".to_vec()]);

        remove_fixture(root);
        Ok(())
    }

    #[test]
    fn rejects_parent_walk_escape() -> Result<()> {
        let root = fixture_root("escape")?;
        let mut server = Server::new(LocalTree::open(&root)?);
        attach(&mut server);
        let reply = server.handle(TMessage::Walk {
            tag: 2,
            fid: 1,
            newfid: 2,
            wnames: vec![b"..".to_vec()],
        });
        assert!(matches!(reply, RMessage::Error { .. }));

        remove_fixture(root);
        Ok(())
    }

    #[test]
    fn serves_symlink_target_without_following_outside_export() -> Result<()> {
        let root = fixture_root("symlink")?;
        let outside = fixture_root("outside")?;
        fs::write(outside.join("secret"), b"secret")
            .map_err(|error| Error::from(error.to_string()))?;
        symlink(outside.join("secret"), root.join("secret-link"))
            .map_err(|error| Error::from(error.to_string()))?;

        let mut server = Server::new(LocalTree::open(&root)?);
        attach(&mut server);
        let reply = server.handle(TMessage::Walk {
            tag: 2,
            fid: 1,
            newfid: 2,
            wnames: vec![b"secret-link".to_vec()],
        });
        assert!(matches!(reply, RMessage::Walk { .. }));
        let reply = server.handle(TMessage::Read {
            tag: 3,
            fid: 2,
            offset: 0,
            count: 8192,
        });
        assert_eq!(
            reply,
            RMessage::Read {
                tag: 3,
                data: outside.join("secret").as_os_str().as_bytes().to_vec()
            }
        );

        remove_fixture(root);
        remove_fixture(outside);
        Ok(())
    }

    #[test]
    fn read_only_export_rejects_writes() -> Result<()> {
        let root = fixture_root("readonly")?;
        fs::write(root.join("body"), b"abcdef").map_err(|error| Error::from(error.to_string()))?;

        let mut server = Server::new(LocalTree::open(&root)?);
        attach(&mut server);
        walk(&mut server, 1, 2, b"body");
        let reply = server.handle(TMessage::Open {
            tag: 3,
            fid: 2,
            mode: OWRITE,
        });
        assert!(matches!(reply, RMessage::Error { .. }));

        remove_fixture(root);
        Ok(())
    }

    #[test]
    fn writable_export_creates_truncates_and_writes() -> Result<()> {
        let root = fixture_root("writable")?;
        fs::write(root.join("body"), b"abcdef").map_err(|error| Error::from(error.to_string()))?;

        let mut server = Server::new(LocalTree::open_with_config(
            &root,
            LocalTreeConfig { writable: true },
        )?);
        attach(&mut server);
        let reply = server.handle(TMessage::Create {
            tag: 2,
            fid: 1,
            name: b"created".to_vec(),
            perm: 0o666,
            mode: OWRITE,
        });
        let created_qid = match reply {
            RMessage::Create { qid, .. } => qid,
            other => return Err(Error::from(format!("unexpected create reply: {other:?}"))),
        };
        let reply = server.handle(TMessage::Write {
            tag: 3,
            fid: 1,
            offset: 0,
            data: b"created\n".to_vec(),
        });
        assert_eq!(reply, RMessage::Write { tag: 3, count: 8 });
        assert_eq!(
            fs::read_to_string(root.join("created"))
                .map_err(|error| Error::from(error.to_string()))?,
            "created\n"
        );
        let reply = server.handle(TMessage::Clunk { tag: 4, fid: 1 });
        assert!(matches!(reply, RMessage::Clunk { .. }));

        attach(&mut server);
        walk(&mut server, 1, 2, b"body");
        let reply = server.handle(TMessage::Open {
            tag: 5,
            fid: 2,
            mode: OWRITE | OTRUNC,
        });
        assert!(matches!(reply, RMessage::Open { .. }));
        let reply = server.handle(TMessage::Write {
            tag: 6,
            fid: 2,
            offset: 0,
            data: b"xy".to_vec(),
        });
        assert_eq!(reply, RMessage::Write { tag: 6, count: 2 });
        assert_eq!(
            fs::read_to_string(root.join("body"))
                .map_err(|error| Error::from(error.to_string()))?,
            "xy"
        );
        assert!(!created_qid.is_dir());

        remove_fixture(root);
        Ok(())
    }

    fn attach(server: &mut Server<LocalTree>) {
        let reply = server.handle(TMessage::Attach {
            tag: 1,
            fid: 1,
            afid: r9p::NOFID,
            uname: b"codex".to_vec(),
            aname: b"/".to_vec(),
        });
        assert!(matches!(reply, RMessage::Attach { .. }));
    }

    fn walk(server: &mut Server<LocalTree>, fid: Fid, newfid: Fid, name: &[u8]) {
        let reply = server.handle(TMessage::Walk {
            tag: 2,
            fid,
            newfid,
            wnames: vec![name.to_vec()],
        });
        assert!(matches!(reply, RMessage::Walk { .. }));
    }

    fn clone_fid(server: &mut Server<LocalTree>, fid: Fid, newfid: Fid) {
        let reply = server.handle(TMessage::Walk {
            tag: 2,
            fid,
            newfid,
            wnames: Vec::new(),
        });
        assert!(matches!(reply, RMessage::Walk { .. }));
    }

    fn open(server: &mut Server<LocalTree>, fid: Fid) {
        let reply = server.handle(TMessage::Open {
            tag: 3,
            fid,
            mode: OREAD,
        });
        assert!(matches!(reply, RMessage::Open { .. }));
    }

    fn fixture_root(label: &str) -> Result<PathBuf> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| Error::from(error.to_string()))?
            .as_nanos();
        let path = env::temp_dir().join(format!("r9p-fs-{label}-{}-{nanos}", process::id()));
        fs::create_dir(&path).map_err(|error| Error::from(error.to_string()))?;
        Ok(path)
    }

    fn remove_fixture(path: PathBuf) {
        let _ = fs::remove_dir_all(path);
    }
}
