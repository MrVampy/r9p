//! `open` / `read` / `write` / `release` op handlers.

use crate::{
    error::{Error, Result},
    fuse::{
        reply::{read_struct, reply_bytes, reply_empty, reply_error, reply_struct},
        util::{flags_to_9p_mode, fuse_open_flags, is_namespace_shape_error, is_transport_error},
        wire::{
            FuseInHeader, FuseOpenIn, FuseOpenOut, FuseReadIn, FuseReleaseIn, FuseWriteIn,
            FuseWriteOut,
        },
        R9pFuse,
    },
    node::{is_dir, read_directory_entries},
    p9::{OREAD, OTRUNC},
};
use std::{fs::File, mem::size_of, thread};

impl R9pFuse {
    pub(in crate::fuse) fn open(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
        payload: &[u8],
        is_dir_open: bool,
    ) -> Result<()> {
        let input = read_struct::<FuseOpenIn>(payload)?;
        let replayable = is_dir_open || flags_to_9p_mode(input.flags) == OREAD;
        match self.open_once(file, header, input, is_dir_open) {
            Ok(()) => Ok(()),
            Err(error) if is_namespace_shape_error(&error) => {
                self.refresh_node(header.nodeid)?;
                self.open_once(file, header, input, is_dir_open)
            }
            Err(error) if is_transport_error(&error) && replayable => {
                self.reconnect()?;
                self.open_once(file, header, input, is_dir_open)
            }
            Err(error) if is_transport_error(&error) => {
                self.reconnect()?;
                Err(Error::new(
                    libc::EIO,
                    "open failed during reconnect; mutating opens are not replayed",
                ))
            }
            Err(error) => Err(error),
        }
    }

    fn open_once(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
        input: FuseOpenIn,
        is_dir_open: bool,
    ) -> Result<()> {
        let node_stat = {
            let nodes = self.nodes()?;
            let node = nodes.node(header.nodeid)?;
            node.stat.clone()
        };
        if is_dir_open && !is_dir(&node_stat) {
            return reply_error(file, header.unique, libc::ENOTDIR);
        }
        let (client, node_fid) = self.bound_node_fid(header.nodeid)?;
        let open_timeout = if flags_to_9p_mode(input.flags) == OREAD || is_dir_open {
            self.lookup_timeout()
        } else {
            self.mutation_timeout()
        };
        let fid = client.clone_fid_timeout(node_fid, open_timeout)?;
        let mut mode = flags_to_9p_mode(input.flags);
        if is_dir_open {
            mode = OREAD;
        }
        if let Err(error) = client.open_timeout(fid, mode, open_timeout) {
            if mode & OTRUNC != 0
                && !is_transport_error(&error)
                && !is_namespace_shape_error(&error)
            {
                mode &= !OTRUNC;
                if let Err(error) = client.open_timeout(fid, mode, open_timeout) {
                    let _ = client.clunk_timeout(fid, self.control_timeout());
                    return Err(error);
                }
            } else {
                let _ = client.clunk_timeout(fid, self.control_timeout());
                return Err(error);
            }
        }
        let dir_entries = if is_dir_open {
            let mut dir_client = client.clone();
            match read_directory_entries(&mut dir_client, node_fid, self.read_timeout()) {
                Ok(entries) => entries,
                Err(error) => {
                    let _ = client.clunk_timeout(fid, self.control_timeout());
                    return Err(error);
                }
            }
        } else {
            Vec::new()
        };
        let handle = self
            .nodes()?
            .open_handle(client.clone(), fid, is_dir_open, dir_entries);
        let out = FuseOpenOut {
            fh: handle,
            open_flags: fuse_open_flags(is_dir_open, mode),
            padding: 0,
        };
        reply_struct(file, header.unique, &out)
    }

    pub(in crate::fuse) fn read(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
        payload: &[u8],
    ) -> Result<()> {
        let input = read_struct::<FuseReadIn>(payload)?;
        let handle = self.nodes()?.handle(input.fh)?.clone();
        let data = match handle.client.read_full_timeout(
            handle.fid,
            input.offset,
            input.size,
            self.read_timeout(),
        ) {
            Ok(data) => data,
            Err(error) if is_transport_error(&error) => {
                self.reconnect()?;
                return Err(Error::new(
                    libc::ESTALE,
                    "file handle is stale after 9P reconnect",
                ));
            }
            Err(error) if is_namespace_shape_error(&error) => {
                self.refresh_node(header.nodeid)?;
                return Err(Error::new(
                    libc::ESTALE,
                    "file handle is stale after namespace refresh",
                ));
            }
            Err(error) => return Err(error),
        };
        reply_bytes(file, header.unique, &data)
    }

    pub(in crate::fuse) fn write(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
        payload: &[u8],
    ) -> Result<()> {
        let input = read_struct::<FuseWriteIn>(payload)?;
        let header_len = size_of::<FuseWriteIn>();
        let size =
            usize::try_from(input.size).map_err(|_| Error::new(libc::EINVAL, "write too large"))?;
        let data = payload
            .get(header_len..header_len.saturating_add(size))
            .ok_or_else(|| Error::new(libc::EINVAL, "truncated FUSE write"))?;
        let node_path = {
            let nodes = self.nodes()?;
            nodes.node(header.nodeid)?.path.clone()
        };
        let handle = self.nodes()?.handle(input.fh)?.clone();
        let write_timeout = if is_namespace_control_write_path(&node_path) {
            self.control_timeout()
        } else {
            self.write_timeout()
        };
        let count = match handle
            .client
            .write_timeout(handle.fid, input.offset, data, write_timeout)
        {
            Ok(count) => count,
            Err(error) if is_transport_error(&error) => {
                self.reconnect()?;
                return Err(Error::new(
                    libc::EIO,
                    "write failed during reconnect and was not replayed",
                ));
            }
            Err(error) if is_namespace_shape_error(&error) => {
                self.refresh_node(header.nodeid)?;
                return Err(Error::new(
                    libc::EIO,
                    "write failed during namespace refresh and was not replayed",
                ));
            }
            Err(error) => return Err(error),
        };
        let stale_fids = if is_namespace_control_write_path(&node_path) {
            self.refresh_path_bindings_after_namespace_change()?
        } else {
            Vec::new()
        };
        let out = FuseWriteOut {
            size: count,
            padding: 0,
        };
        reply_struct(file, header.unique, &out)?;
        if !stale_fids.is_empty() {
            if let Ok(client) = self.client_snapshot() {
                let timeout = self.control_timeout();
                thread::spawn(move || {
                    for fid in stale_fids {
                        let _ = client.clunk_timeout(fid, timeout);
                    }
                });
            }
        }
        Ok(())
    }

    pub(in crate::fuse) fn release(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
        payload: &[u8],
    ) -> Result<()> {
        let input = read_struct::<FuseReleaseIn>(payload)?;
        let handle = self.nodes()?.remove_handle(input.fh);
        if let Some(handle) = handle {
            let _ = handle
                .client
                .clunk_timeout(handle.fid, self.control_timeout());
        }
        reply_empty(file, header.unique)
    }
}

fn is_namespace_control_write_path(path: &[Vec<u8>]) -> bool {
    path_matches(path, &[b"runtime", b"namespaces", b"current", b"mount"])
        || path_matches(path, &[b"runtime", b"namespaces", b"current", b"unmount"])
        || is_worktree_control_write_path(path)
}

fn is_worktree_control_write_path(path: &[Vec<u8>]) -> bool {
    path.len() == 4
        && path[0].as_slice() == b"runtime"
        && path[1].as_slice() == b"worktrees"
        && !path[2].is_empty()
        && path[3].as_slice() == b"ctl"
}

fn path_matches(path: &[Vec<u8>], expected: &[&[u8]]) -> bool {
    path.len() == expected.len()
        && path
            .iter()
            .zip(expected.iter())
            .all(|(left, right)| left.as_slice() == *right)
}

#[cfg(test)]
mod tests {
    use super::is_namespace_control_write_path;

    fn path(segments: &[&[u8]]) -> Vec<Vec<u8>> {
        segments.iter().map(|segment| segment.to_vec()).collect()
    }

    #[test]
    fn worktree_ctl_writes_refresh_namespace_bindings() {
        assert!(is_namespace_control_write_path(&path(&[
            b"runtime",
            b"worktrees",
            b"wt-plan45",
            b"ctl",
        ])));
    }

    #[test]
    fn worktree_children_do_not_all_refresh_namespace_bindings() {
        assert!(!is_namespace_control_write_path(&path(&[
            b"runtime",
            b"worktrees",
            b"wt-plan45",
            b"status",
        ])));
    }
}
