//! Incremental directory read handling. The opened 9P fid is the stable
//! snapshot boundary. Each FUSE handle retains decoded entries and advances
//! the 9P byte offset only when a requested FUSE buffer reaches the end of
//! what that handle has already observed.
//!
//! READDIRPLUS lets us return entry attributes alongside each name, so
//! Linux's dcache is populated without follow-up LOOKUP+GETATTR round
//! trips. We seed FUSE nodeids from the 9P stat data and bind a 9P fid
//! lazily only when a later operation needs one.

use crate::{
    error::{Error, Result},
    fuse::{
        reply::{as_bytes, push_u32, push_u64, read_struct, reply_bytes},
        util::dirent_size,
        util::{is_namespace_shape_error, is_transport_error},
        wire::{FuseEntryOut, FuseInHeader, FuseReadIn},
        R9pFuse,
    },
    node::{is_dir, qid_to_inode, DirEntry},
};
use r9p::blocking::DEFAULT_READ_CHUNK;
use session::validate_directory_entries;
use std::{fs::File, mem::size_of};

impl R9pFuse {
    pub(in crate::fuse) fn readdir(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
        payload: &[u8],
    ) -> Result<()> {
        let input = read_struct::<FuseReadIn>(payload)?;
        let size = usize::try_from(input.size)
            .map_err(|_| Error::new(libc::EINVAL, "readdir too large"))?;
        let entries = self.directory_entries_for_read_with_recovery(
            header.nodeid,
            input.fh,
            input.offset,
            size,
            DirectoryEncoding::Plain,
        )?;
        let data = self.encode_dirents(header.nodeid, input.offset, size, &entries)?;
        reply_bytes(file, header.unique, &data)
    }

    pub(in crate::fuse) fn readdirplus(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
        payload: &[u8],
    ) -> Result<()> {
        let input = read_struct::<FuseReadIn>(payload)?;
        let size = usize::try_from(input.size)
            .map_err(|_| Error::new(libc::EINVAL, "readdirplus too large"))?;
        let entries = self.directory_entries_for_read_with_recovery(
            header.nodeid,
            input.fh,
            input.offset,
            size,
            DirectoryEncoding::Plus,
        )?;
        let data = self.encode_dirents_plus(header.nodeid, input.offset, size, &entries)?;
        reply_bytes(file, header.unique, &data)
    }

    fn directory_entries_for_read_with_recovery(
        &mut self,
        nodeid: u64,
        handle_id: u64,
        offset: u64,
        size: usize,
        encoding: DirectoryEncoding,
    ) -> Result<Vec<DirEntry>> {
        match self.directory_entries_for_read(handle_id, offset, size, encoding) {
            Ok(entries) => Ok(entries),
            Err(error) if is_transport_error(&error) => {
                self.reconnect()?;
                self.reopen_directory_handle(nodeid, handle_id)?;
                self.directory_entries_for_read(handle_id, offset, size, encoding)
            }
            Err(error) if is_namespace_shape_error(&error) => {
                self.recover_namespace_shape(nodeid)?;
                self.reopen_directory_handle(nodeid, handle_id)?;
                self.directory_entries_for_read(handle_id, offset, size, encoding)
            }
            Err(error) => Err(error),
        }
    }

    fn reopen_directory_handle(&mut self, nodeid: u64, handle_id: u64) -> Result<()> {
        let (client, node_fid) = self.bound_node_fid(nodeid)?;
        let fid = client.clone_fid_timeout(node_fid, self.lookup_timeout())?;
        if let Err(error) = client.open_timeout(fid, session::OREAD, self.lookup_timeout()) {
            let _ = client.clunk_timeout(fid, self.control_timeout());
            return Err(error.into());
        }
        let (old_client, old_fid) =
            match self
                .nodes()?
                .replace_directory_handle_binding(handle_id, client.clone(), fid)
            {
                Ok(replaced) => replaced,
                Err(error) => {
                    let _ = client.clunk_timeout(fid, self.control_timeout());
                    return Err(error);
                }
            };
        let _ = old_client.clunk_timeout(old_fid, self.control_timeout());
        Ok(())
    }

    fn directory_entries_for_read(
        &self,
        handle_id: u64,
        offset: u64,
        size: usize,
        encoding: DirectoryEncoding,
    ) -> Result<Vec<DirEntry>> {
        let handle = self.nodes()?.handle(handle_id)?.clone();
        if !handle.is_dir {
            return Err(Error::new(libc::ENOTDIR, "file handle is not a directory"));
        }
        let directory = handle.directory.ok_or_else(|| {
            Error::new(libc::ESTALE, "directory handle has no incremental stream")
        })?;
        let mut stream = directory
            .lock()
            .map_err(|_| Error::new(libc::EIO, "directory stream lock poisoned"))?;
        while !stream.eof && !directory_buffer_satisfied(offset, size, &stream.entries, encoding) {
            let requested = size
                .max(4096)
                .min(usize::try_from(DEFAULT_READ_CHUNK).unwrap_or(usize::MAX));
            let count = u32::try_from(requested)
                .map_err(|_| Error::new(libc::EINVAL, "directory read too large"))?;
            let chunk = stream.client.read_timeout(
                stream.fid,
                stream.remote_offset,
                count,
                self.read_timeout(),
            )?;
            if chunk.is_empty() {
                stream.eof = true;
                break;
            }
            let validated = validate_directory_entries(&stream.client, &chunk)?;
            stream.remote_offset = stream
                .remote_offset
                .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
            stream.entries.extend(validated);
        }
        Ok(stream.entries.clone())
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
                None => self.special_direntplus_entry(parent_nodeid, name)?,
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

    fn special_direntplus_entry(
        &self,
        parent_nodeid: u64,
        name: &[u8],
    ) -> Result<(FuseEntryOut, u32, u64)> {
        let nodeid = {
            let nodes = self.nodes()?;
            if name == b".." {
                nodes.parent_nodeid(parent_nodeid)?
            } else {
                parent_nodeid
            }
        };
        let (generation, qid, stat) = {
            let nodes = self.nodes()?;
            let node = nodes.node(nodeid)?;
            (node.generation, node.qid, node.stat.clone())
        };
        Ok((
            self.entry_out(nodeid, generation, &stat),
            libc::DT_DIR as u32,
            qid_to_inode(qid),
        ))
    }

    fn bind_child(&self, parent_nodeid: u64, entry: &DirEntry) -> Result<u64> {
        let mut nodes = self.nodes()?;
        nodes.insert_lookup_lazy(parent_nodeid, entry.stat.clone(), &entry.name)
    }

    fn encode_dirents(
        &self,
        parent_nodeid: u64,
        offset: u64,
        size: usize,
        entries: &[DirEntry],
    ) -> Result<Vec<u8>> {
        let (dot_ino, dotdot_ino) = self.special_dirent_inodes(parent_nodeid)?;
        encode_dirents(dot_ino, dotdot_ino, offset, size, entries)
    }

    fn special_dirent_inodes(&self, parent_nodeid: u64) -> Result<(u64, u64)> {
        let nodes = self.nodes()?;
        let dot = nodes.node(parent_nodeid)?;
        let dotdot = nodes.node(nodes.parent_nodeid(parent_nodeid)?)?;
        Ok((qid_to_inode(dot.qid), qid_to_inode(dotdot.qid)))
    }
}

#[derive(Clone, Copy)]
enum DirectoryEncoding {
    Plain,
    Plus,
}

fn directory_buffer_satisfied(
    offset: u64,
    size: usize,
    entries: &[DirEntry],
    encoding: DirectoryEncoding,
) -> bool {
    if size == 0 {
        return true;
    }
    let start = usize::try_from(offset).unwrap_or(usize::MAX);
    let total = entries.len().saturating_add(2);
    if start >= total {
        return false;
    }
    let mut used = 0usize;
    for index in start..total {
        let name_len = match index {
            0 => 1,
            1 => 2,
            current => entries[current - 2].name.len(),
        };
        let needed = match encoding {
            DirectoryEncoding::Plain => dirent_size(name_len),
            DirectoryEncoding::Plus => direntplus_size(name_len),
        };
        if used.saturating_add(needed) > size {
            return true;
        }
        used = used.saturating_add(needed);
        if used == size {
            return true;
        }
    }
    false
}

pub(in crate::fuse) fn encode_dirents(
    dot_ino: u64,
    dotdot_ino: u64,
    offset: u64,
    size: usize,
    entries: &[DirEntry],
) -> Result<Vec<u8>> {
    let mut logical = Vec::with_capacity(entries.len() + 2);
    logical.push((b".".to_vec(), dot_ino, libc::DT_DIR as u32));
    logical.push((b"..".to_vec(), dotdot_ino, libc::DT_DIR as u32));
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
    size_of::<FuseEntryOut>() + dirent_size(name_len)
}

#[cfg(test)]
mod tests {
    use super::{directory_buffer_satisfied, DirectoryEncoding};
    use crate::{fuse::util::dirent_size, node::DirEntry};
    use r9p::{qid::Qid, stat::Stat};

    fn entry(name: &str, path: u64) -> DirEntry {
        let stat = Stat::new(name, Qid::file(path), 0o444);
        DirEntry {
            name: name.as_bytes().to_vec(),
            qid: stat.qid,
            stat,
        }
    }

    #[test]
    fn an_empty_remote_directory_is_needed_after_dot_entries() {
        let dot_bytes = dirent_size(1) + dirent_size(2);
        assert!(!directory_buffer_satisfied(
            0,
            dot_bytes + dirent_size(5),
            &[],
            DirectoryEncoding::Plain,
        ));
        assert!(!directory_buffer_satisfied(
            2,
            dirent_size(5),
            &[],
            DirectoryEncoding::Plain,
        ));
    }

    #[test]
    fn cached_entries_stop_an_unnecessary_remote_read() {
        let entries = vec![entry("alpha", 10), entry("beta", 11)];
        assert!(directory_buffer_satisfied(
            2,
            dirent_size(5),
            &entries,
            DirectoryEncoding::Plain,
        ));
        assert!(!directory_buffer_satisfied(
            4,
            dirent_size(5),
            &entries,
            DirectoryEncoding::Plain,
        ));
    }
}
