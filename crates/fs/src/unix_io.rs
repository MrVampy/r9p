use r9p::{
    error::{Error, Result, EEXIST, ENOENT, EPERM},
    mode,
    qid::{Qid, DMDIR, DMSYMLINK, QTFILE, QTSYMLINK},
    stat::Stat,
    OEXEC, ORCLOSE, ORDWR, OREAD, OTRUNC, OWRITE,
};
use std::{
    ffi::{CStr, CString, OsStr},
    fs,
    mem::MaybeUninit,
    os::{
        fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd},
        unix::ffi::OsStrExt,
    },
    path::{Path, PathBuf},
};

const EMFILE_PROTOCOL: &str = "too many open files";

pub(super) struct Node {
    pub(super) fd: OwnedFd,
    pub(super) stat: Stat,
}

impl Node {
    pub(super) fn duplicate(&self) -> Result<Self> {
        Ok(Self {
            fd: duplicate_fd(self.fd.as_raw_fd())?,
            stat: self.stat.clone(),
        })
    }
}

pub(super) fn is_read_only_mode(open_mode: u8) -> bool {
    mode::is_valid(open_mode)
        && matches!(open_mode & mode::ACCESS_MASK, OREAD | OEXEC)
        && open_mode & (OTRUNC | ORCLOSE) == 0
}

fn validate_local_open_mode(open_mode: u8) -> Result<()> {
    if mode::is_valid(open_mode) && open_mode & ORCLOSE == 0 {
        Ok(())
    } else {
        Err(Error::from_static(EPERM))
    }
}

fn libc_open_flags(open_mode: u8) -> Result<libc::c_int> {
    validate_local_open_mode(open_mode)?;
    let access = match open_mode & mode::ACCESS_MASK {
        OREAD | OEXEC => libc::O_RDONLY,
        OWRITE => libc::O_WRONLY,
        ORDWR => libc::O_RDWR,
        _ => return Err(Error::from_static(EPERM)),
    };
    Ok(access
        | libc::O_CLOEXEC
        | if open_mode & OTRUNC != 0 {
            libc::O_TRUNC
        } else {
            0
        })
}

pub(super) fn open_root(root: &Path) -> Result<OwnedFd> {
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

pub(super) fn open_child(parent: RawFd, name: &[u8]) -> Result<Node> {
    if name == b"." || name == b".." {
        return Err(Error::from_static(ENOENT));
    }
    let c_name = CString::new(name).map_err(|_| Error::from_static(ENOENT))?;
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

pub(super) fn mkdir_child(parent: RawFd, name: &[u8], perm: u32) -> Result<()> {
    let c_name = child_name(name)?;
    let status = unsafe { libc::mkdirat(parent, c_name.as_ptr(), (perm & 0o777) as libc::mode_t) };
    if status == 0 {
        Ok(())
    } else {
        Err(map_io("mkdirat", std::io::Error::last_os_error()))
    }
}

pub(super) fn create_file_fd(parent: RawFd, name: &[u8], perm: u32, mode: u8) -> Result<OwnedFd> {
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
        return Err(Error::from_static(ENOENT));
    }
    CString::new(name).map_err(|_| Error::from_static(ENOENT))
}

pub(super) fn node_from_fd(fd: OwnedFd, name: Vec<u8>) -> Result<Node> {
    let st = fstat(fd.as_raw_fd())?;
    let kind = st.st_mode & libc::S_IFMT;
    if kind != libc::S_IFREG && kind != libc::S_IFDIR && kind != libc::S_IFLNK {
        return Err(Error::from_static(ENOENT));
    }
    let stat = stat_from_libc(&st, name);
    Ok(Node { fd, stat })
}

pub(super) fn stat_from_libc(st: &libc::stat, name: Vec<u8>) -> Stat {
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

pub(super) fn is_symlink(stat: &Stat) -> bool {
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

pub(super) fn read_dir(path_fd: RawFd) -> Result<Vec<Stat>> {
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

pub(super) fn pread_file(fd: RawFd, offset: u64, count: u32) -> Result<Vec<u8>> {
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

pub(super) fn read_link(fd: RawFd) -> Result<Vec<u8>> {
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

pub(super) fn open_read_fd(path_fd: RawFd, directory: bool) -> Result<OwnedFd> {
    let proc_path = format!("/proc/self/fd/{path_fd}");
    let c_path = CString::new(proc_path).map_err(|_| Error::from("proc fd path contains NUL"))?;
    let mut flags = libc::O_RDONLY | libc::O_CLOEXEC;
    if directory {
        flags |= libc::O_DIRECTORY;
    }
    let raw = unsafe { libc::open(c_path.as_ptr(), flags) };
    owned_fd(raw).map_err(|error| map_io("open proc fd", error))
}

pub(super) fn open_file_fd(path_fd: RawFd, mode: u8) -> Result<OwnedFd> {
    let proc_path = format!("/proc/self/fd/{path_fd}");
    let c_path = CString::new(proc_path).map_err(|_| Error::from("proc fd path contains NUL"))?;
    let raw = unsafe { libc::open(c_path.as_ptr(), libc_open_flags(mode)?) };
    owned_fd(raw).map_err(|error| map_io("open proc fd", error))
}

pub(super) fn pwrite_file(fd: RawFd, offset: u64, data: &[u8]) -> Result<u32> {
    let written =
        unsafe { libc::pwrite(fd, data.as_ptr().cast(), data.len(), offset as libc::off_t) };
    if written < 0 {
        return Err(map_io("pwrite", std::io::Error::last_os_error()));
    }
    u32::try_from(written).map_err(|_| Error::from("write count overflow"))
}

pub(super) fn truncate_fd(path_fd: RawFd, length: u64) -> Result<()> {
    validate_truncate_length(length)?;
    let file = open_file_fd(path_fd, OWRITE)?;
    let status = unsafe { libc::ftruncate(file.as_raw_fd(), length as libc::off_t) };
    if status == 0 {
        Ok(())
    } else {
        Err(map_io("ftruncate", std::io::Error::last_os_error()))
    }
}

pub(super) fn remove_path(path_fd: RawFd, is_dir: bool) -> Result<()> {
    let path = proc_fd_path(path_fd)?;
    let result = if is_dir {
        fs::remove_dir(&path)
    } else {
        fs::remove_file(&path)
    };
    result.map_err(|error| map_io("remove path", error))
}

pub(super) fn rename_path(path_fd: RawFd, new_name: &[u8]) -> Result<()> {
    validate_rename_name(new_name)?;
    let source = proc_fd_path(path_fd)?;
    let parent = source.parent().ok_or_else(|| Error::from_static(ENOENT))?;
    let target = parent.join(OsStr::from_bytes(new_name));
    rename_no_replace(&source, &target)
}

pub(super) fn validate_rename_name(new_name: &[u8]) -> Result<()> {
    if new_name.is_empty()
        || new_name == b"."
        || new_name == b".."
        || new_name.contains(&b'/')
        || new_name.contains(&0)
    {
        Err(Error::from_static(ENOENT))
    } else {
        Ok(())
    }
}

pub(super) fn validate_truncate_length(length: u64) -> Result<()> {
    if length > libc::off_t::MAX as u64 {
        Err(Error::from("file length exceeds host off_t"))
    } else {
        Ok(())
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn rename_no_replace(source: &Path, target: &Path) -> Result<()> {
    let source =
        CString::new(source.as_os_str().as_bytes()).map_err(|_| Error::from_static(ENOENT))?;
    let target =
        CString::new(target.as_os_str().as_bytes()).map_err(|_| Error::from_static(ENOENT))?;
    let status = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if status == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EEXIST) {
        Err(Error::from_static(EEXIST))
    } else {
        Err(map_io("rename path", error))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn rename_no_replace(source: &Path, target: &Path) -> Result<()> {
    match fs::symlink_metadata(target) {
        Ok(_) => return Err(Error::from_static(EEXIST)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(map_io("inspect rename target", error)),
    }
    fs::rename(source, target).map_err(|error| map_io("rename path", error))
}

fn proc_fd_path(path_fd: RawFd) -> Result<PathBuf> {
    fs::read_link(format!("/proc/self/fd/{path_fd}"))
        .map_err(|error| map_io("read proc fd link", error))
}

pub(super) fn duplicate_fd(fd: RawFd) -> Result<OwnedFd> {
    let raw = unsafe { libc::dup(fd) };
    owned_fd(raw).map_err(|error| map_io("dup fd", error))
}

pub(super) fn fstat(fd: RawFd) -> Result<libc::stat> {
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
        Some(libc::ENOENT | libc::ENOTDIR | libc::ELOOP) => Error::from_static(ENOENT),
        Some(libc::EACCES | libc::EPERM) => Error::from_static(EPERM),
        Some(libc::EMFILE | libc::ENFILE) => Error::from_static(EMFILE_PROTOCOL),
        _ => Error::from(format!("{context}: {error}")),
    }
}
