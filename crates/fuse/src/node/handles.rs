use super::{DirEntry, NodeTable};
use crate::{
    error::{Error, Result},
    p9::Client,
};
use r9p::fid::Fid;
use std::fmt;

#[derive(Clone)]
pub struct Handle {
    pub client: Client,
    pub fid: Fid,
    pub is_dir: bool,
    pub write_on_release: bool,
    pub close_commit: bool,
    pub close_commit_flushed: bool,
    pub bytes_written: u64,
    pub dir_entries: Vec<DirEntry>,
}

impl fmt::Debug for Handle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Handle")
            .field("fid", &self.fid)
            .field("is_dir", &self.is_dir)
            .field("write_on_release", &self.write_on_release)
            .field("close_commit", &self.close_commit)
            .field("close_commit_flushed", &self.close_commit_flushed)
            .field("bytes_written", &self.bytes_written)
            .field("dir_entries", &self.dir_entries)
            .finish()
    }
}

impl NodeTable {
    pub fn open_handle(
        &mut self,
        client: Client,
        fid: Fid,
        is_dir: bool,
        write_on_release: bool,
        close_commit: bool,
        dir_entries: Vec<DirEntry>,
    ) -> u64 {
        let handle = self.next_handle;
        self.next_handle = self.next_handle.saturating_add(1).max(1);
        self.handles.insert(
            handle,
            Handle {
                client,
                fid,
                is_dir,
                write_on_release,
                close_commit,
                close_commit_flushed: false,
                bytes_written: 0,
                dir_entries,
            },
        );
        handle
    }

    pub fn handle(&self, handle: u64) -> Result<&Handle> {
        self.handles
            .get(&handle)
            .ok_or_else(|| Error::new(libc::ESTALE, format!("unknown file handle {handle}")))
    }

    pub fn replace_read_handle_binding(
        &mut self,
        handle: u64,
        client: Client,
        fid: Fid,
    ) -> Result<Handle> {
        let current = self
            .handles
            .get_mut(&handle)
            .ok_or_else(|| Error::new(libc::ESTALE, format!("unknown file handle {handle}")))?;
        if current.is_dir || current.write_on_release {
            return Err(Error::new(
                libc::ESTALE,
                "file handle is not read-only replayable",
            ));
        }
        let old = current.clone();
        current.client = client;
        current.fid = fid;
        Ok(old)
    }

    pub fn replace_write_handle_binding(
        &mut self,
        handle: u64,
        client: Client,
        fid: Fid,
    ) -> Result<Handle> {
        let current = self
            .handles
            .get_mut(&handle)
            .ok_or_else(|| Error::new(libc::ESTALE, format!("unknown file handle {handle}")))?;
        if current.is_dir || !current.write_on_release {
            return Err(Error::new(
                libc::ESTALE,
                "file handle is not write replayable",
            ));
        }
        let old = current.clone();
        current.client = client;
        current.fid = fid;
        current.close_commit_flushed = false;
        Ok(old)
    }

    pub fn note_handle_write(&mut self, handle: u64, count: u32) -> Result<()> {
        let handle = self
            .handles
            .get_mut(&handle)
            .ok_or_else(|| Error::new(libc::ESTALE, format!("unknown file handle {handle}")))?;
        handle.bytes_written = handle.bytes_written.saturating_add(u64::from(count));
        Ok(())
    }

    pub fn mark_close_commit_flushed(&mut self, handle: u64) -> Result<()> {
        let handle = self
            .handles
            .get_mut(&handle)
            .ok_or_else(|| Error::new(libc::ESTALE, format!("unknown file handle {handle}")))?;
        handle.close_commit_flushed = true;
        Ok(())
    }

    pub fn remove_handle(&mut self, handle: u64) -> Option<Handle> {
        self.handles.remove(&handle)
    }
}
