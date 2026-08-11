use super::{DirEntry, NodeTable};
use crate::error::{Error, Result};
use r9p::fid::Fid;
use session::Client;
use std::fmt;
use std::sync::{Arc, Mutex};

pub(crate) struct DirectoryStream {
    pub(crate) client: Client,
    pub(crate) fid: Fid,
    pub(crate) remote_offset: u64,
    pub(crate) entries: Vec<DirEntry>,
    pub(crate) eof: bool,
}

#[derive(Clone)]
pub struct Handle {
    pub client: Client,
    pub fid: Option<Fid>,
    pub open_mode: u8,
    pub is_dir: bool,
    pub write_on_release: bool,
    pub close_commit: bool,
    pub close_commit_flushed: bool,
    pub bytes_written: u64,
    pub(crate) directory: Option<Arc<Mutex<DirectoryStream>>>,
}

pub(crate) struct OpenHandle {
    pub client: Client,
    pub fid: Option<Fid>,
    pub open_mode: u8,
    pub is_dir: bool,
    pub write_on_release: bool,
    pub close_commit: bool,
    pub directory: Option<DirectoryStream>,
}

impl fmt::Debug for Handle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Handle")
            .field("fid", &self.fid)
            .field("open_mode", &self.open_mode)
            .field("is_dir", &self.is_dir)
            .field("write_on_release", &self.write_on_release)
            .field("close_commit", &self.close_commit)
            .field("close_commit_flushed", &self.close_commit_flushed)
            .field("bytes_written", &self.bytes_written)
            .field("has_directory_stream", &self.directory.is_some())
            .finish()
    }
}

impl Handle {
    pub fn require_fid(&self) -> Result<Fid> {
        self.fid
            .ok_or_else(|| Error::new(libc::ESTALE, "file handle has no 9P fid"))
    }
}

impl NodeTable {
    pub(crate) fn open_handle(&mut self, open: OpenHandle) -> u64 {
        let handle = self.next_handle;
        self.next_handle = self.next_handle.saturating_add(1).max(1);
        self.handles.insert(
            handle,
            Handle {
                client: open.client,
                fid: open.fid,
                open_mode: open.open_mode,
                is_dir: open.is_dir,
                write_on_release: open.write_on_release,
                close_commit: open.close_commit,
                close_commit_flushed: false,
                bytes_written: 0,
                directory: open
                    .directory
                    .map(|directory| Arc::new(Mutex::new(directory))),
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
        current.fid = Some(fid);
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
        current.fid = Some(fid);
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
