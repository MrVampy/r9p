use super::Client;
use crate::{Error, Result};
use r9p::{
    qid::{Qid, DMDIR},
    stat::{decode_dir_entries, Stat},
    ORDWR, OREAD, OTRUNC, OWRITE,
};

const PATH_READ_CHUNK: u32 = 64 * 1024;

impl Client {
    pub fn stat_path(&self, path: &str) -> Result<Stat> {
        let fid = self.walk_path(path)?;
        let result = self.stat(fid);
        finish_fid(self, fid, result)
    }

    pub fn list_path(&self, path: &str) -> Result<Vec<Stat>> {
        let fid = self.walk_path(path)?;
        let result = (|| {
            let stat = self.stat(fid)?;
            if stat.mode & DMDIR == 0 {
                return Err(Error::new(libc::ENOTDIR, "not a directory"));
            }
            self.open(fid, OREAD)?;
            decode_dir_entries(&read_all(self, fid)?)
                .map_err(|error| Error::new(libc::EPROTO, error.display_lossy().to_string()))
        })();
        finish_fid(self, fid, result)
    }

    pub fn read_path(&self, path: &str) -> Result<Vec<u8>> {
        let fid = self.walk_path(path)?;
        let result = (|| {
            self.open(fid, OREAD)?;
            read_all(self, fid)
        })();
        finish_fid(self, fid, result)
    }

    pub fn read_path_range(&self, path: &str, offset: u64, count: u32) -> Result<Vec<u8>> {
        let fid = self.walk_path(path)?;
        let result = (|| {
            self.open(fid, OREAD)?;
            self.read(fid, offset, count)
        })();
        finish_fid(self, fid, result)
    }

    pub fn write_path(&self, path: &str, offset: u64, data: &[u8]) -> Result<u32> {
        let fid = self.walk_path(path)?;
        let result = (|| {
            self.open(fid, OWRITE)?;
            self.write(fid, offset, data)
        })();
        finish_fid(self, fid, result)
    }

    pub fn write_file(&self, path: &str, data: &[u8]) -> Result<u32> {
        let fid = self.walk_path(path)?;
        let result = (|| {
            self.open(fid, OWRITE | OTRUNC)?;
            self.write(fid, 0, data)
        })();
        finish_fid(self, fid, result)
    }

    pub fn rpc_path(&self, path: &str, request: &[u8]) -> Result<Vec<u8>> {
        let fid = self.walk_path(path)?;
        let result = (|| {
            self.open(fid, ORDWR)?;
            let count = self.write(fid, 0, request)?;
            if usize::try_from(count).ok() != Some(request.len()) {
                return Err(Error::new(libc::EIO, "short RPC request write"));
            }
            read_all(self, fid)
        })();
        finish_fid(self, fid, result)
    }

    pub fn create_at(&self, parent: &str, name: &str, perm: u32, mode: u8) -> Result<Qid> {
        let parent_fid = self.walk_path(parent)?;
        let result = self
            .create(parent_fid, name.as_bytes(), perm, mode)
            .and_then(|(fid, qid)| {
                self.clunk(fid)?;
                Ok(qid)
            });
        finish_fid(self, parent_fid, result)
    }

    pub fn create_write_at(
        &self,
        parent: &str,
        name: &str,
        perm: u32,
        mode: u8,
        offset: u64,
        data: &[u8],
    ) -> Result<u32> {
        let parent_fid = self.walk_path(parent)?;
        let result = self
            .create(parent_fid, name.as_bytes(), perm, mode)
            .and_then(|(fid, _)| {
                let write = self.write(fid, offset, data);
                finish_fid(self, fid, write)
            });
        finish_fid(self, parent_fid, result)
    }

    pub fn remove_path(&self, path: &str) -> Result<()> {
        let fid = self.walk_path(path)?;
        self.remove(fid)
    }
}

fn read_all(client: &Client, fid: r9p::Fid) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut offset = 0_u64;
    loop {
        let chunk = client.read(fid, offset, PATH_READ_CHUNK)?;
        if chunk.is_empty() {
            return Ok(bytes);
        }
        offset = offset
            .checked_add(
                u64::try_from(chunk.len())
                    .map_err(|_| Error::new(libc::EOVERFLOW, "read length overflow"))?,
            )
            .ok_or_else(|| Error::new(libc::EOVERFLOW, "read offset overflow"))?;
        bytes.extend(chunk);
    }
}

fn finish_fid<T>(client: &Client, fid: r9p::Fid, result: Result<T>) -> Result<T> {
    let clunk = client.clunk(fid);
    match (result, clunk) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(error)) | (Err(error), _) => Err(error),
    }
}
