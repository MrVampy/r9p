use super::Client;
use crate::{Error, Result};
use r9p::{
    qid::{Qid, DMDIR},
    stat::{decode_dir_entries, Stat},
    ORDWR, OREAD, OTRUNC, OWRITE,
};
use std::time::Duration;

const PATH_READ_CHUNK: u32 = 64 * 1024;

impl Client {
    pub fn stat_path_timeout(&self, path: &str, timeout: Duration) -> Result<Stat> {
        read_only_path_timeout(self, path, timeout, |client, fid| {
            client.stat_timeout(fid, timeout)
        })
    }

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

    pub fn read_path_timeout(
        &self,
        path: &str,
        max_response_bytes: u32,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        validate_response_bound(max_response_bytes)?;
        read_only_path_timeout(self, path, timeout, |client, fid| {
            client.open_timeout(fid, OREAD, timeout)?;
            client.read_full_timeout(fid, 0, max_response_bytes, timeout)
        })
    }

    pub fn read_path_delimited_timeout(
        &self,
        path: &str,
        max_response_bytes: u32,
        delimiter: u8,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        validate_response_bound(max_response_bytes)?;
        read_only_path_timeout(self, path, timeout, |client, fid| {
            client.open_timeout(fid, OREAD, timeout)?;
            client.read_delimited_timeout(fid, 0, max_response_bytes, delimiter, timeout)
        })
    }

    /// Reads one delimiter-terminated record from a path whose read may block.
    ///
    /// Walk, open, recovery, and clunk remain bounded by `control_timeout`.
    /// The record read itself has no deadline and can be cancelled by shutting
    /// down the client session. A definitive failed referral is re-established
    /// and this read-only path operation is retried once.
    pub fn read_path_delimited(
        &self,
        path: &str,
        max_response_bytes: u32,
        delimiter: u8,
        control_timeout: Duration,
    ) -> Result<Vec<u8>> {
        validate_response_bound(max_response_bytes)?;
        read_only_path_timeout(self, path, control_timeout, |client, fid| {
            client.open_timeout(fid, OREAD, control_timeout)?;
            client.read_delimited(fid, 0, max_response_bytes, delimiter)
        })
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

    pub fn write_file_timeout(&self, path: &str, data: &[u8], timeout: Duration) -> Result<u32> {
        let mut file = self.open_path_timeout(path, OWRITE | OTRUNC, timeout)?;
        let written = file.write_timeout(0, data, timeout)?;
        file.close()?;
        Ok(written)
    }

    /// Replaces one file or creates it when it is definitively absent.
    ///
    /// This is intended for desired-state publication whose full contents are
    /// safe to submit after an `ENOENT` or `EEXIST` response. It never retries
    /// an ambiguous write failure. A concurrent creator is resolved by one
    /// final replacement attempt.
    pub fn reconcile_file_at(
        &self,
        parent: &str,
        name: &str,
        perm: u32,
        data: &[u8],
    ) -> Result<u32> {
        let (target_parent, leaf) = create_target(parent, name)?;
        let target_path = child_path(&target_parent, leaf);
        match self.write_file(&target_path, data) {
            Ok(count) => Ok(count),
            Err(error) if error.errno == libc::ENOENT => {
                match self.create_write_at(&target_parent, leaf, perm, OWRITE, 0, data) {
                    Ok(count) => Ok(count),
                    Err(error) if error.errno == libc::EEXIST => {
                        self.write_file(&target_path, data)
                    }
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        }
    }

    /// Bounded form of [`Self::reconcile_file_at`].
    ///
    /// Every transport operation uses `timeout`. Definitive absence or an
    /// `EEXIST` create race may select the next valid operation, but an
    /// ambiguous write or create failure is returned without replay.
    pub fn reconcile_file_at_timeout(
        &self,
        parent: &str,
        name: &str,
        perm: u32,
        data: &[u8],
        timeout: Duration,
    ) -> Result<u32> {
        let (target_parent, leaf) = create_target(parent, name)?;
        let target_path = child_path(&target_parent, leaf);
        match self.write_file_timeout(&target_path, data, timeout) {
            Ok(count) => Ok(count),
            Err(error) if error.errno == libc::ENOENT => {
                match self.create_write_at_timeout(
                    &target_parent,
                    leaf,
                    perm,
                    OWRITE,
                    0,
                    data,
                    timeout,
                ) {
                    Ok(count) => Ok(count),
                    Err(error) if error.errno == libc::EEXIST => {
                        self.write_file_timeout(&target_path, data, timeout)
                    }
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        }
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

    pub fn rpc_path_timeout(
        &self,
        path: &str,
        request: &[u8],
        max_response_bytes: u32,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        validate_response_bound(max_response_bytes)?;
        let mut fid = self.open_path_timeout(path, ORDWR, timeout)?;
        let written = fid.write_timeout(0, request, timeout)?;
        if usize::try_from(written).ok() != Some(request.len()) {
            return Err(Error::new(
                libc::EIO,
                format!(
                    "RPC file accepted {written} of {} request bytes",
                    request.len()
                ),
            ));
        }
        let response = fid.read_full_timeout(0, max_response_bytes, timeout)?;
        fid.close()?;
        Ok(response)
    }

    /// Creates a child below `parent`.
    ///
    /// `name` may be one canonical relative path. The namespace client walks
    /// every parent component and sends only the final element in `Tcreate`.
    pub fn create_at(&self, parent: &str, name: &str, perm: u32, mode: u8) -> Result<Qid> {
        let (parent, name) = create_target(parent, name)?;
        let parent_fid = self.walk_path(&parent)?;
        let result = self
            .create(parent_fid, name.as_bytes(), perm, mode)
            .and_then(|(fid, qid)| {
                self.clunk(fid)?;
                Ok(qid)
            });
        finish_fid(self, parent_fid, result)
    }

    /// Creates a child below `parent` with one deadline for walk, create, and
    /// fid cleanup.
    ///
    /// Like [`Self::create_at`], `name` may be one canonical relative path.
    /// A timeout after `Tcreate` was sent is an ambiguous mutation result; this
    /// helper does not replay the create.
    pub fn create_at_timeout(
        &self,
        parent: &str,
        name: &str,
        perm: u32,
        mode: u8,
        timeout: Duration,
    ) -> Result<Qid> {
        let (parent, name) = create_target(parent, name)?;
        let parent_fid = self.walk_path_timeout(&parent, timeout)?;
        let result = self
            .create_timeout(parent_fid, name.as_bytes(), perm, mode, timeout)
            .and_then(|(fid, qid)| {
                self.clunk_timeout(fid, timeout)?;
                Ok(qid)
            });
        finish_fid_timeout(self, parent_fid, timeout, result)
    }

    /// Creates a child below `parent` and writes its initial contents.
    ///
    /// `name` has the same canonical relative-path behavior as [`Self::create_at`].
    pub fn create_write_at(
        &self,
        parent: &str,
        name: &str,
        perm: u32,
        mode: u8,
        offset: u64,
        data: &[u8],
    ) -> Result<u32> {
        let (parent, name) = create_target(parent, name)?;
        let parent_fid = self.walk_path(&parent)?;
        let result = self
            .create(parent_fid, name.as_bytes(), perm, mode)
            .and_then(|(fid, _)| {
                let write = self.write(fid, offset, data);
                finish_fid(self, fid, write)
            });
        finish_fid(self, parent_fid, result)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_write_at_timeout(
        &self,
        parent: &str,
        name: &str,
        perm: u32,
        mode: u8,
        offset: u64,
        data: &[u8],
        timeout: Duration,
    ) -> Result<u32> {
        let (parent, name) = create_target(parent, name)?;
        let parent_fid = self.walk_path_timeout(&parent, timeout)?;
        let result = self
            .create_timeout(parent_fid, name.as_bytes(), perm, mode, timeout)
            .and_then(|(fid, _)| {
                let write = self.write_timeout(fid, offset, data, timeout);
                finish_fid_timeout(self, fid, timeout, write)
            });
        finish_fid_timeout(self, parent_fid, timeout, result)
    }

    pub fn remove_path(&self, path: &str) -> Result<()> {
        let fid = self.walk_path(path)?;
        self.remove(fid)
    }
}

fn read_only_path_timeout<T>(
    client: &Client,
    path: &str,
    timeout: Duration,
    operation: impl Fn(&Client, r9p::Fid) -> Result<T>,
) -> Result<T> {
    let mut retry_available = true;
    loop {
        let fid = client.walk_path_timeout(path, timeout)?;
        let (route_client, route_mount) = client.route_failure_context(fid)?;
        let result = operation(client, fid);
        let result = finish_fid_timeout(client, fid, timeout, result);
        match result {
            Err(error)
                if retry_available
                    && client.recover_read_only_route(
                        fid,
                        &route_client,
                        route_mount.as_deref(),
                        &error,
                        timeout,
                    )? =>
            {
                retry_available = false;
            }
            result => return result,
        }
    }
}

fn create_target<'a>(parent: &str, name: &'a str) -> Result<(String, &'a str)> {
    let _ = super::parse_namespace_path(parent.as_bytes())?;
    let Some((relative_parent, leaf)) = name.rsplit_once('/') else {
        return Ok((parent.to_string(), name));
    };
    if name.starts_with('/') {
        return Err(Error::new(
            libc::EINVAL,
            "create path must be relative to its parent",
        ));
    }
    let _ = super::parse_namespace_path(name.as_bytes())?;
    let target_parent = if parent == "/" {
        format!("/{relative_parent}")
    } else {
        format!("{parent}/{relative_parent}")
    };
    Ok((target_parent, leaf))
}

fn child_path(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else {
        format!("{parent}/{name}")
    }
}

fn validate_response_bound(max_response_bytes: u32) -> Result<()> {
    if max_response_bytes == 0 {
        return Err(Error::new(
            libc::EINVAL,
            "path response bound must be nonzero",
        ));
    }
    Ok(())
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

fn finish_fid_timeout<T>(
    client: &Client,
    fid: r9p::Fid,
    timeout: Duration,
    result: Result<T>,
) -> Result<T> {
    let clunk = client.clunk_timeout(fid, timeout);
    match (result, clunk) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(error)) | (Err(error), _) => Err(error),
    }
}
