use std::time::Duration;

use r9p::{multiplex::DelimitedRead, Fid};

use crate::{Client, Error, Result};

/// An opened 9P fid retained for repeated, ordered operations.
///
/// Keeping the fid alive avoids repeating walk/open/clunk for a hot namespace
/// file. Methods require mutable access so callers cannot accidentally issue
/// overlapping operations whose file-level ordering would be ambiguous.
pub struct OpenedFid {
    client: Client,
    fid: Option<Fid>,
    clunk_timeout: Duration,
}

impl Client {
    pub fn open_path_timeout(&self, path: &str, mode: u8, timeout: Duration) -> Result<OpenedFid> {
        let names = path_names(path)?;
        let fid = if names.is_empty() {
            self.clone_fid_timeout(self.root_fid(), timeout)?
        } else {
            self.walk_timeout(self.root_fid(), &names, timeout)?
        };
        if let Err(error) = self.open_timeout(fid, mode, timeout) {
            let _ = self.clunk_timeout(fid, timeout);
            return Err(error);
        }
        Ok(OpenedFid {
            client: self.clone(),
            fid: Some(fid),
            clunk_timeout: timeout,
        })
    }
}

impl OpenedFid {
    pub fn read_timeout(&mut self, offset: u64, count: u32, timeout: Duration) -> Result<Vec<u8>> {
        self.client
            .read_timeout(self.required_fid()?, offset, count, timeout)
    }

    pub fn read_full_timeout(
        &mut self,
        offset: u64,
        count: u32,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        self.client
            .read_full_timeout(self.required_fid()?, offset, count, timeout)
    }

    pub fn read_delimited_timeout(
        &mut self,
        offset: u64,
        count: u32,
        delimiter: u8,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        self.client
            .read_delimited_timeout(self.required_fid()?, offset, count, delimiter, timeout)
    }

    pub fn read_full(&mut self, offset: u64, count: u32) -> Result<Vec<u8>> {
        self.client.read_full(self.required_fid()?, offset, count)
    }

    pub fn read_delimited(&mut self, offset: u64, count: u32, delimiter: u8) -> Result<Vec<u8>> {
        self.client
            .read_delimited(self.required_fid()?, offset, count, delimiter)
    }

    pub fn write_timeout(&mut self, offset: u64, data: &[u8], timeout: Duration) -> Result<u32> {
        self.client
            .write_timeout(self.required_fid()?, offset, data, timeout)
    }

    /// Pipelines the final request write with the first response read on this
    /// retained fid and returns one bounded delimiter-terminated record.
    pub fn write_then_read_delimited_timeout(
        &mut self,
        write_offset: u64,
        data: &[u8],
        read: DelimitedRead,
        timeout: Duration,
    ) -> Result<(u32, Vec<u8>)> {
        self.client.write_then_read_delimited_timeout(
            self.required_fid()?,
            write_offset,
            data,
            read,
            timeout,
        )
    }

    pub fn close(mut self) -> Result<()> {
        self.clunk()
    }

    fn required_fid(&self) -> Result<Fid> {
        self.fid
            .ok_or_else(|| Error::new(libc::EBADF, "opened fid is closed"))
    }

    fn clunk(&mut self) -> Result<()> {
        let Some(fid) = self.fid.take() else {
            return Ok(());
        };
        self.client.clunk_timeout(fid, self.clunk_timeout)
    }
}

impl Drop for OpenedFid {
    fn drop(&mut self) {
        let _ = self.clunk();
    }
}

fn path_names(path: &str) -> Result<Vec<Vec<u8>>> {
    if path.is_empty() || path == "/" {
        return Ok(Vec::new());
    }
    let path = path.strip_prefix('/').unwrap_or(path);
    if path.is_empty() || path.ends_with('/') {
        return Err(Error::new(libc::EINVAL, "invalid 9P path"));
    }
    path.split('/')
        .map(|name| {
            if name.is_empty() {
                Err(Error::new(libc::EINVAL, "invalid 9P path"))
            } else {
                Ok(name.as_bytes().to_vec())
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::path_names;

    #[test]
    fn path_names_accept_root_absolute_and_relative_paths() {
        assert_eq!(path_names("/").expect("root"), Vec::<Vec<u8>>::new());
        assert_eq!(
            path_names("/agents/status").expect("absolute"),
            vec![b"agents".to_vec(), b"status".to_vec()]
        );
        assert_eq!(
            path_names("agents/status").expect("relative"),
            vec![b"agents".to_vec(), b"status".to_vec()]
        );
    }

    #[test]
    fn path_names_reject_empty_segments() {
        assert!(path_names("/agents//status").is_err());
        assert!(path_names("/agents/status/").is_err());
    }
}
