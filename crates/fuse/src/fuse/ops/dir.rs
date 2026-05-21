//! Directory read handling. Plan 9 returns a stable directory listing at
//! open time; we cache it on the handle and serve FUSE READDIR /
//! READDIRPLUS slices from that snapshot.
//!
//! READDIRPLUS lets us return entry attributes alongside each name, so
//! Linux's dcache is populated without follow-up LOOKUP+GETATTR round
//! trips. We seed FUSE nodeids from the 9P stat data and bind a 9P fid
//! lazily only when a later operation needs one.

use crate::{
    error::{Error, Result},
    fuse::{
        reply::{as_bytes, push_u32, push_u64, read_struct, reply_bytes, reply_error},
        util::dirent_size,
        wire::{FuseEntryOut, FuseInHeader, FuseReadIn},
        R9pFuse,
    },
    node::{is_dir, qid_to_inode, DirEntry, ROOT_NODEID},
};
use std::{fs::File, mem::size_of};

impl R9pFuse {
    pub(in crate::fuse) fn readdir(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
        payload: &[u8],
    ) -> Result<()> {
        let input = read_struct::<FuseReadIn>(payload)?;
        let handle = self.nodes()?.handle(input.fh)?.clone();
        if !handle.is_dir {
            return reply_error(file, header.unique, libc::ENOTDIR);
        }
        let size = usize::try_from(input.size)
            .map_err(|_| Error::new(libc::EINVAL, "readdir too large"))?;
        let data = encode_dirents(header.nodeid, input.offset, size, &handle.dir_entries)?;
        reply_bytes(file, header.unique, &data)
    }

    pub(in crate::fuse) fn readdirplus(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
        payload: &[u8],
    ) -> Result<()> {
        let input = read_struct::<FuseReadIn>(payload)?;
        let handle = self.nodes()?.handle(input.fh)?.clone();
        if !handle.is_dir {
            return reply_error(file, header.unique, libc::ENOTDIR);
        }
        let size = usize::try_from(input.size)
            .map_err(|_| Error::new(libc::EINVAL, "readdirplus too large"))?;
        let data =
            self.encode_dirents_plus(header.nodeid, input.offset, size, &handle.dir_entries)?;
        reply_bytes(file, header.unique, &data)
    }

    fn encode_dirents_plus(
        &mut self,
        parent_nodeid: u64,
        offset: u64,
        size: usize,
        entries: &[DirEntry],
    ) -> Result<Vec<u8>> {
        let total = entries.len().saturating_add(2);
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        let mut out = Vec::new();
        for index in start..total {
            let (name, real) = match index {
                0 => (b"." as &[u8], None),
                1 => (b".." as &[u8], None),
                i => {
                    let entry = &entries[i - 2];
                    (entry.name.as_slice(), Some(entry))
                }
            };
            let needed = direntplus_size(name.len());
            if out.len().saturating_add(needed) > size {
                break;
            }
            let (entry_out, kind, ino) = match real {
                None => (zero_entry_out(), libc::DT_DIR as u32, 0_u64),
                Some(entry) => {
                    let nodeid = self.bind_child(parent_nodeid, entry)?;
                    let generation = self.nodes()?.node(nodeid)?.generation;
                    let entry_out = self.entry_out(nodeid, generation, &entry.stat);
                    let kind = if is_dir(&entry.stat) {
                        libc::DT_DIR as u32
                    } else {
                        libc::DT_REG as u32
                    };
                    (entry_out, kind, qid_to_inode(entry.qid))
                }
            };
            let next_offset = u64::try_from(index + 1).unwrap_or(u64::MAX);
            out.extend(as_bytes(&entry_out));
            push_u64(&mut out, ino);
            push_u64(&mut out, next_offset);
            push_u32(
                &mut out,
                u32::try_from(name.len())
                    .map_err(|_| Error::new(libc::EINVAL, "directory name too long"))?,
            );
            push_u32(&mut out, kind);
            out.extend(name);
            while out.len() % 8 != 0 {
                out.push(0);
            }
        }
        Ok(out)
    }

    fn bind_child(&self, parent_nodeid: u64, entry: &DirEntry) -> Result<u64> {
        let mut nodes = self.nodes()?;
        nodes.insert_lookup_lazy(parent_nodeid, entry.stat.clone(), &entry.name)
    }
}

pub(in crate::fuse) fn encode_dirents(
    parent_nodeid: u64,
    offset: u64,
    size: usize,
    entries: &[DirEntry],
) -> Result<Vec<u8>> {
    let mut logical = Vec::with_capacity(entries.len() + 2);
    logical.push((
        b".".to_vec(),
        if parent_nodeid == ROOT_NODEID {
            ROOT_NODEID
        } else {
            parent_nodeid
        },
        libc::DT_DIR as u32,
    ));
    logical.push((b"..".to_vec(), ROOT_NODEID, libc::DT_DIR as u32));
    for entry in entries {
        logical.push((
            entry.name.clone(),
            qid_to_inode(entry.qid),
            if is_dir(&entry.stat) {
                libc::DT_DIR as u32
            } else {
                libc::DT_REG as u32
            },
        ));
    }

    let mut out = Vec::new();
    let start = usize::try_from(offset).unwrap_or(usize::MAX);
    for (index, (name, ino, kind)) in logical.into_iter().enumerate().skip(start) {
        let next_offset = u64::try_from(index + 1).unwrap_or(u64::MAX);
        let needed = dirent_size(name.len());
        if out.len().saturating_add(needed) > size {
            break;
        }
        push_u64(&mut out, ino);
        push_u64(&mut out, next_offset);
        push_u32(
            &mut out,
            u32::try_from(name.len())
                .map_err(|_| Error::new(libc::EINVAL, "directory name too long"))?,
        );
        push_u32(&mut out, kind);
        out.extend(name);
        while out.len() % 8 != 0 {
            out.push(0);
        }
    }
    Ok(out)
}

fn direntplus_size(name_len: usize) -> usize {
    // entry_out + dirent header (ino+off+namelen+type) + name, padded to 8.
    let prefix = size_of::<FuseEntryOut>() + 8 + 8 + 4 + 4;
    (prefix + name_len + 7) & !7
}

fn zero_entry_out() -> FuseEntryOut {
    FuseEntryOut {
        nodeid: 0,
        generation: 0,
        entry_valid: 0,
        attr_valid: 0,
        entry_valid_nsec: 0,
        attr_valid_nsec: 0,
        attr: zero_attr(),
    }
}

fn zero_attr() -> crate::fuse::wire::FuseAttr {
    crate::fuse::wire::FuseAttr {
        ino: 0,
        size: 0,
        blocks: 0,
        atime: 0,
        mtime: 0,
        ctime: 0,
        atimensec: 0,
        mtimensec: 0,
        ctimensec: 0,
        mode: 0,
        nlink: 0,
        uid: 0,
        gid: 0,
        rdev: 0,
        blksize: 0,
        flags: 0,
    }
}
