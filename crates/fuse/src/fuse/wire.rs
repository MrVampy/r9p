//! FUSE wire-protocol structures and opcode constants.
//!
//! The struct shapes here mirror Linux kernel `fuse_kernel.h` at protocol
//! version 7.31 (Linux 5.4). That version is widely available, predates the
//! `FUSE_INIT_EXT`/`flags2` extension envelope, and unlocks the capabilities
//! r9p mount cares about: `ATOMIC_O_TRUNC`, `EXPORT_SUPPORT`, `DONT_MASK`,
//! `AUTO_INVAL_DATA`, `PARALLEL_DIROPS`, and `READDIRPLUS`.

pub(super) const FUSE_LOOKUP: u32 = 1;
pub(super) const FUSE_FORGET: u32 = 2;
pub(super) const FUSE_GETATTR: u32 = 3;
pub(super) const FUSE_SETATTR: u32 = 4;
pub(super) const FUSE_SYMLINK: u32 = 6;
pub(super) const FUSE_MKNOD: u32 = 8;
pub(super) const FUSE_MKDIR: u32 = 9;
pub(super) const FUSE_UNLINK: u32 = 10;
pub(super) const FUSE_RMDIR: u32 = 11;
pub(super) const FUSE_RENAME: u32 = 12;
pub(super) const FUSE_LINK: u32 = 13;
pub(super) const FUSE_OPEN: u32 = 14;
pub(super) const FUSE_READ: u32 = 15;
pub(super) const FUSE_WRITE: u32 = 16;
pub(super) const FUSE_STATFS: u32 = 17;
pub(super) const FUSE_RELEASE: u32 = 18;
pub(super) const FUSE_FSYNC: u32 = 20;
pub(super) const FUSE_SETXATTR: u32 = 21;
pub(super) const FUSE_GETXATTR: u32 = 22;
pub(super) const FUSE_LISTXATTR: u32 = 23;
pub(super) const FUSE_REMOVEXATTR: u32 = 24;
pub(super) const FUSE_FLUSH: u32 = 25;
pub(super) const FUSE_INIT: u32 = 26;
pub(super) const FUSE_OPENDIR: u32 = 27;
pub(super) const FUSE_READDIR: u32 = 28;
pub(super) const FUSE_RELEASEDIR: u32 = 29;
pub(super) const FUSE_FSYNCDIR: u32 = 30;
pub(super) const FUSE_GETLK: u32 = 31;
pub(super) const FUSE_SETLK: u32 = 32;
pub(super) const FUSE_SETLKW: u32 = 33;
pub(super) const FUSE_ACCESS: u32 = 34;
pub(super) const FUSE_CREATE: u32 = 35;
pub(super) const FUSE_INTERRUPT: u32 = 36;
pub(super) const FUSE_DESTROY: u32 = 38;
pub(super) const FUSE_POLL: u32 = 40;
pub(super) const FUSE_BATCH_FORGET: u32 = 42;
pub(super) const FUSE_READDIRPLUS: u32 = 44;

pub(super) const FATTR_MODE: u32 = 1 << 0;
pub(super) const FATTR_UID: u32 = 1 << 1;
pub(super) const FATTR_GID: u32 = 1 << 2;
pub(super) const FATTR_SIZE: u32 = 1 << 3;
pub(super) const FATTR_ATIME: u32 = 1 << 4;
pub(super) const FATTR_MTIME: u32 = 1 << 5;
pub(super) const FATTR_FH: u32 = 1 << 6;

pub(super) const FUSE_ASYNC_READ: u32 = 1 << 0;
pub(super) const FUSE_ATOMIC_O_TRUNC: u32 = 1 << 3;
pub(super) const FUSE_EXPORT_SUPPORT: u32 = 1 << 4;
pub(super) const FUSE_BIG_WRITES: u32 = 1 << 5;
pub(super) const FUSE_DONT_MASK: u32 = 1 << 6;
pub(super) const FUSE_AUTO_INVAL_DATA: u32 = 1 << 12;
pub(super) const FUSE_DO_READDIRPLUS: u32 = 1 << 13;
pub(super) const FUSE_PARALLEL_DIROPS: u32 = 1 << 18;

pub(super) const FUSE_NOTIFY_INVAL_INODE: i32 = 2;
pub(super) const FUSE_NOTIFY_INVAL_ENTRY: i32 = 3;

pub(super) const FOPEN_DIRECT_IO: u32 = 1 << 0;

pub(super) const FUSE_KERNEL_VERSION: u32 = 7;
pub(super) const FUSE_KERNEL_MINOR_VERSION: u32 = 31;
pub(super) const FUSE_COMPAT_INIT_OUT_SIZE: usize = 8;
pub(super) const FUSE_COMPAT_22_INIT_OUT_SIZE: usize = 24;
pub(super) const DEFAULT_MAX_WRITE: u32 = 1024 * 1024;
pub(super) const FUSE_BUFFER_SIZE: usize = 1024 * 1024 + 8192;

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FuseInHeader {
    pub(super) len: u32,
    pub(super) opcode: u32,
    pub(super) unique: u64,
    pub(super) nodeid: u64,
    pub(super) uid: u32,
    pub(super) gid: u32,
    pub(super) pid: u32,
    pub(super) total_extlen: u16,
    pub(super) padding: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FuseOutHeader {
    pub(super) len: u32,
    pub(super) error: i32,
    pub(super) unique: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FuseAttr {
    pub(super) ino: u64,
    pub(super) size: u64,
    pub(super) blocks: u64,
    pub(super) atime: u64,
    pub(super) mtime: u64,
    pub(super) ctime: u64,
    pub(super) atimensec: u32,
    pub(super) mtimensec: u32,
    pub(super) ctimensec: u32,
    pub(super) mode: u32,
    pub(super) nlink: u32,
    pub(super) uid: u32,
    pub(super) gid: u32,
    pub(super) rdev: u32,
    pub(super) blksize: u32,
    pub(super) flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FuseEntryOut {
    pub(super) nodeid: u64,
    pub(super) generation: u64,
    pub(super) entry_valid: u64,
    pub(super) attr_valid: u64,
    pub(super) entry_valid_nsec: u32,
    pub(super) attr_valid_nsec: u32,
    pub(super) attr: FuseAttr,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FuseAttrOut {
    pub(super) attr_valid: u64,
    pub(super) attr_valid_nsec: u32,
    pub(super) dummy: u32,
    pub(super) attr: FuseAttr,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FuseForgetIn {
    pub(super) nlookup: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FuseForgetOne {
    pub(super) nodeid: u64,
    pub(super) nlookup: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FuseBatchForgetIn {
    pub(super) count: u32,
    pub(super) dummy: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FuseMknodIn {
    pub(super) mode: u32,
    pub(super) rdev: u32,
    pub(super) umask: u32,
    pub(super) padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FuseMkdirIn {
    pub(super) mode: u32,
    pub(super) umask: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FuseRenameIn {
    pub(super) newdir: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FuseSetattrIn {
    pub(super) valid: u32,
    pub(super) padding: u32,
    pub(super) fh: u64,
    pub(super) size: u64,
    pub(super) lock_owner: u64,
    pub(super) atime: u64,
    pub(super) mtime: u64,
    pub(super) ctime: u64,
    pub(super) atimensec: u32,
    pub(super) mtimensec: u32,
    pub(super) ctimensec: u32,
    pub(super) mode: u32,
    pub(super) unused4: u32,
    pub(super) uid: u32,
    pub(super) gid: u32,
    pub(super) unused5: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FuseOpenIn {
    pub(super) flags: u32,
    pub(super) open_flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FuseCreateIn {
    pub(super) flags: u32,
    pub(super) mode: u32,
    pub(super) umask: u32,
    pub(super) open_flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FuseOpenOut {
    pub(super) fh: u64,
    pub(super) open_flags: u32,
    pub(super) padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FuseCreateOut {
    pub(super) entry: FuseEntryOut,
    pub(super) open: FuseOpenOut,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FuseReleaseIn {
    pub(super) fh: u64,
    pub(super) flags: u32,
    pub(super) release_flags: u32,
    pub(super) lock_owner: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FuseReadIn {
    pub(super) fh: u64,
    pub(super) offset: u64,
    pub(super) size: u32,
    pub(super) read_flags: u32,
    pub(super) lock_owner: u64,
    pub(super) flags: u32,
    pub(super) padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FuseWriteIn {
    pub(super) fh: u64,
    pub(super) offset: u64,
    pub(super) size: u32,
    pub(super) write_flags: u32,
    pub(super) lock_owner: u64,
    pub(super) flags: u32,
    pub(super) padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FuseWriteOut {
    pub(super) size: u32,
    pub(super) padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FuseInterruptIn {
    pub(super) unique: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FuseNotifyInvalInodeOut {
    pub(super) ino: u64,
    pub(super) off: i64,
    pub(super) len: i64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FuseNotifyInvalEntryOut {
    pub(super) parent: u64,
    pub(super) namelen: u32,
    pub(super) flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FusePollIn {
    pub(super) fh: u64,
    pub(super) kh: u64,
    pub(super) flags: u32,
    pub(super) events: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FusePollOut {
    pub(super) revents: u32,
    pub(super) padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FuseFileLock {
    pub(super) start: u64,
    pub(super) end: u64,
    pub(super) type_: u32,
    pub(super) pid: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FuseLkIn {
    pub(super) fh: u64,
    pub(super) owner: u64,
    pub(super) lk: FuseFileLock,
    pub(super) lk_flags: u32,
    pub(super) padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FuseLkOut {
    pub(super) lk: FuseFileLock,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FuseKstatfs {
    pub(super) blocks: u64,
    pub(super) bfree: u64,
    pub(super) bavail: u64,
    pub(super) files: u64,
    pub(super) ffree: u64,
    pub(super) bsize: u32,
    pub(super) namelen: u32,
    pub(super) frsize: u32,
    pub(super) padding: u32,
    pub(super) spare: [u32; 6],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FuseStatfsOut {
    pub(super) st: FuseKstatfs,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FuseInitIn {
    pub(super) major: u32,
    pub(super) minor: u32,
    pub(super) max_readahead: u32,
    pub(super) flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FuseInitOut {
    pub(super) major: u32,
    pub(super) minor: u32,
    pub(super) max_readahead: u32,
    pub(super) flags: u32,
    pub(super) max_background: u16,
    pub(super) congestion_threshold: u16,
    pub(super) max_write: u32,
    pub(super) time_gran: u32,
    pub(super) max_pages: u16,
    pub(super) map_alignment: u16,
    pub(super) unused: [u32; 8],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FuseDirent {
    pub(super) ino: u64,
    pub(super) off: u64,
    pub(super) namelen: u32,
    pub(super) type_: u32,
    pub(super) name: [u8; 1],
}
