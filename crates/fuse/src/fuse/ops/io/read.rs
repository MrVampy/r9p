use crate::{
    error::{Error, Result},
    fuse::{
        read_cache::CacheIdentity,
        reply::{read_struct, reply_bytes, reply_error},
        util::{is_namespace_shape_error, is_transport_error},
        wire::{FuseInHeader, FuseReadIn},
        R9pFuse,
    },
    node::is_symlink,
};
use session::OREAD;
use std::fs::File;

const MAX_PARALLEL_CACHE_READS: u32 = 8;

impl R9pFuse {
    pub(in crate::fuse) fn readlink(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
    ) -> Result<()> {
        let stat = {
            let nodes = self.nodes()?;
            nodes.node(header.nodeid)?.stat.clone()
        };
        if !is_symlink(&stat) {
            return reply_error(file, header.unique, libc::EINVAL);
        }
        let (client, fid) = self.bound_node_fid(header.nodeid)?;
        let count = symlink_read_count(&stat)?;
        let data = match client.read_full_timeout(fid, 0, count, self.read_timeout()) {
            Ok(data) => data,
            Err(error) if is_transport_error(&error) => {
                self.reconnect()?;
                return Err(Error::new(
                    libc::ESTALE,
                    "symlink handle is stale after 9P reconnect",
                ));
            }
            Err(error) if is_namespace_shape_error(&error) => {
                self.recover_namespace_shape(header.nodeid)?;
                return Err(Error::new(
                    libc::ESTALE,
                    "symlink handle is stale after namespace refresh",
                ));
            }
            Err(error) => return Err(error.into()),
        };
        reply_bytes(file, header.unique, &data)
    }

    pub(in crate::fuse) fn read(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
        payload: &[u8],
    ) -> Result<()> {
        let input = read_struct::<FuseReadIn>(payload)?;
        let (stat, cache_identity) = {
            let nodes = self.nodes()?;
            let node = nodes.node(header.nodeid)?;
            let identity = read_cache_identity(&node.stat, node.stat_freshness.is_stale());
            (node.stat.clone(), identity)
        };
        let known_length = stat.length;
        if read_is_known_eof(known_length, input.offset) {
            return reply_bytes(file, header.unique, &[]);
        }
        let count = read_count(known_length, input.offset, input.size);
        let handle = self.nodes()?.handle(input.fh)?.clone();
        let fid = handle.require_fid()?;
        let data = match self.read_handle_range(
            &handle.client,
            fid,
            handle.open_mode,
            &stat,
            cache_identity,
            input.offset,
            count,
        ) {
            Ok(data) => data,
            Err(error)
                if is_transport_error(&error)
                    && read_handle_is_replayable(handle.is_dir, handle.write_on_release) =>
            {
                self.reconnect()?;
                self.read_from_reopened_handle(header.nodeid, input.fh, input.offset, count)?
            }
            Err(error) if is_transport_error(&error) => {
                self.reconnect()?;
                return Err(Error::new(libc::ESTALE, "file handle is not replayable"));
            }
            Err(error)
                if is_namespace_shape_error(&error)
                    && read_handle_is_replayable(handle.is_dir, handle.write_on_release) =>
            {
                self.recover_namespace_shape(header.nodeid)?;
                self.read_from_reopened_handle(header.nodeid, input.fh, input.offset, count)?
            }
            Err(error) if is_namespace_shape_error(&error) => {
                self.recover_namespace_shape(header.nodeid)?;
                return Err(Error::new(libc::ESTALE, "file handle is not replayable"));
            }
            Err(error) => return Err(error.into()),
        };
        reply_bytes(file, header.unique, &data)
    }

    fn read_from_reopened_handle(
        &mut self,
        nodeid: u64,
        handle_id: u64,
        offset: u64,
        size: u32,
    ) -> Result<Vec<u8>> {
        let (client, fid, stat) = self.reopen_read_binding(nodeid)?;
        let old_handle =
            match self
                .nodes()?
                .replace_read_handle_binding(handle_id, client.clone(), fid)
            {
                Ok(old_handle) => old_handle,
                Err(error) => {
                    let _ = client.clunk_timeout(fid, self.control_timeout());
                    return Err(error);
                }
            };
        let _ = old_handle
            .client
            .clunk_timeout(old_handle.require_fid()?, self.control_timeout());
        let cache_identity = read_cache_identity(&stat, false);
        self.read_handle_range(&client, fid, OREAD, &stat, cache_identity, offset, size)
    }

    fn read_handle_range(
        &self,
        client: &session::Client,
        fid: r9p::fid::Fid,
        open_mode: u8,
        stat: &r9p::stat::Stat,
        cache_identity: Option<CacheIdentity>,
        offset: u64,
        size: u32,
    ) -> Result<Vec<u8>> {
        if open_mode == OREAD {
            if let (Some(cache), Some(identity)) = (self.read_cache.as_ref(), cache_identity) {
                let result = cache.read(identity, offset, size, |range_offset, range_size| {
                    Ok(client.read_exact_parallel_timeout(
                        fid,
                        range_offset,
                        range_size,
                        MAX_PARALLEL_CACHE_READS,
                        self.read_timeout(),
                    )?)
                });
                self.status.set_read_cache(cache.snapshot());
                return result;
            }
        }
        if stat.length > 0 {
            Ok(client.read_full_timeout(fid, offset, size, self.read_timeout())?)
        } else {
            Ok(client.read_timeout(fid, offset, size, self.read_timeout())?)
        }
    }
}

fn symlink_read_count(stat: &r9p::stat::Stat) -> Result<u32> {
    let count = stat.length.clamp(1, 1024 * 1024);
    u32::try_from(count).map_err(|_| Error::new(libc::EINVAL, "symlink target too large"))
}

fn read_is_known_eof(known_length: u64, offset: u64) -> bool {
    known_length > 0 && offset >= known_length
}

fn read_count(known_length: u64, offset: u64, requested: u32) -> u32 {
    if known_length == 0 {
        return requested;
    }
    let remaining = u32::try_from(known_length.saturating_sub(offset)).unwrap_or(u32::MAX);
    requested.min(remaining)
}

fn read_handle_is_replayable(is_dir: bool, write_on_release: bool) -> bool {
    !is_dir && !write_on_release
}

fn read_cache_identity(stat: &r9p::stat::Stat, stale: bool) -> Option<CacheIdentity> {
    if stale {
        None
    } else {
        CacheIdentity::from_stat(stat)
    }
}

#[cfg(test)]
mod tests {
    use super::{read_cache_identity, read_count, read_handle_is_replayable, read_is_known_eof};
    use r9p::{qid::Qid, stat::Stat};

    #[test]
    fn read_at_known_positive_length_is_eof() {
        assert!(read_is_known_eof(26_698, 26_698));
        assert!(read_is_known_eof(26_698, 30_000));
    }

    #[test]
    fn unknown_zero_length_does_not_short_circuit_dynamic_reads() {
        assert!(!read_is_known_eof(0, 0));
        assert!(!read_is_known_eof(0, 32));
    }

    #[test]
    fn read_before_known_length_reaches_9p() {
        assert!(!read_is_known_eof(26_698, 26_697));
    }

    #[test]
    fn known_file_reads_fill_only_the_remaining_range() {
        assert_eq!(read_count(26_698, 0, 4096), 4096);
        assert_eq!(read_count(26_698, 26_000, 4096), 698);
        assert_eq!(read_count(0, 26_000, 4096), 4096);
    }

    #[test]
    fn only_read_only_file_handles_are_replayable() {
        assert!(read_handle_is_replayable(false, false));
        assert!(!read_handle_is_replayable(false, true));
        assert!(!read_handle_is_replayable(true, false));
    }

    #[test]
    fn namespace_invalidation_bypasses_persistent_cache_until_stat_refresh() {
        let mut stat = Stat::new("video.mp4", Qid::new(0, 4, 7), 0o444);
        stat.length = 32;
        assert!(read_cache_identity(&stat, false).is_some());
        assert!(read_cache_identity(&stat, true).is_none());
    }
}
