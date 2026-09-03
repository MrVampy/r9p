use crate::{
    diagnostics::{DiagnosticContext, DiagnosticRecord},
    error::Result,
    node::{mode_kind, qid_to_inode, StaleBinding},
};
use r9p::stat::Stat;
use session::Client;
use std::{
    fs::File,
    thread,
    time::{Duration, Instant},
};

use super::wire::{FuseAttr, FuseAttrOut, FuseEntryOut};
use super::{
    invalidation::{notify_kernel_invalidations, KernelInvalidation},
    reply,
    util::duration_parts,
    wire, R9pFuse,
};

pub(super) fn source_binding(
    client: &Client,
    source_path: &[Vec<u8>],
    timeout: Duration,
) -> Result<(r9p::fid::Fid, Stat)> {
    let fid = if source_path.is_empty() {
        client.root_fid()
    } else {
        client.walk_timeout(client.root_fid(), source_path, timeout)?
    };
    match client.stat_timeout(fid, timeout) {
        Ok(stat) => Ok((fid, stat)),
        Err(error) => {
            if !source_path.is_empty() {
                let _ = client.clunk_timeout(fid, timeout);
            }
            Err(error.into())
        }
    }
}

pub(super) fn negative_entry_out(timeout: Duration) -> FuseEntryOut {
    let (entry_valid, entry_valid_nsec) = duration_parts(timeout);
    FuseEntryOut {
        nodeid: 0,
        generation: 0,
        entry_valid,
        attr_valid: 0,
        entry_valid_nsec,
        attr_valid_nsec: 0,
        attr: FuseAttr::default(),
    }
}

impl R9pFuse {
    pub(in crate::fuse) fn path_from_source(&self, relative: &[Vec<u8>]) -> Vec<Vec<u8>> {
        let mut path = Vec::with_capacity(self.source_path.len() + relative.len());
        path.extend(self.source_path.iter().cloned());
        path.extend(relative.iter().cloned());
        path
    }

    pub(in crate::fuse) fn source_binding(
        &self,
        client: &Client,
        timeout: Duration,
    ) -> Result<(r9p::fid::Fid, Stat)> {
        source_binding(client, &self.source_path, timeout)
    }

    pub(in crate::fuse) fn walk_from_source(
        &self,
        client: &Client,
        relative: &[Vec<u8>],
        timeout: Duration,
    ) -> Result<r9p::fid::Fid> {
        let path = self.path_from_source(relative);
        if path.is_empty() {
            return Ok(client.clone_fid_timeout(client.root_fid(), timeout)?);
        }
        Ok(client.walk_timeout(client.root_fid(), &path, timeout)?)
    }

    pub(in crate::fuse) fn entry_out(
        &self,
        nodeid: u64,
        generation: u64,
        stat: &Stat,
    ) -> FuseEntryOut {
        let (entry_valid, entry_valid_nsec) = duration_parts(self.config.entry_timeout);
        let (attr_valid, attr_valid_nsec) = duration_parts(self.config.attr_timeout);
        FuseEntryOut {
            nodeid,
            generation,
            entry_valid,
            attr_valid,
            entry_valid_nsec,
            attr_valid_nsec,
            attr: self.attr(stat),
        }
    }

    pub(in crate::fuse) fn negative_entry_out(&self) -> FuseEntryOut {
        negative_entry_out(self.config.negative_timeout)
    }

    pub(in crate::fuse) fn attr_out(&self, stat: &Stat) -> FuseAttrOut {
        let (attr_valid, attr_valid_nsec) = duration_parts(self.config.attr_timeout);
        FuseAttrOut {
            attr_valid,
            attr_valid_nsec,
            dummy: 0,
            attr: self.attr(stat),
        }
    }

    pub(in crate::fuse) fn bound_node_fid(
        &mut self,
        nodeid: u64,
    ) -> Result<(Client, r9p::fid::Fid)> {
        let (path, existing, needs_rebind) = {
            let nodes = self.nodes()?;
            let node = nodes.node(nodeid)?;
            (node.path.clone(), node.fid, node.needs_rebind)
        };
        let client = self.client.snapshot()?;
        match (existing, needs_rebind) {
            (Some(fid), false) => {
                if self.config.debug {
                    eprintln!("r9p mount: node {nodeid} uses cached fid {fid}");
                }
                Ok((client, fid))
            }
            _ => {
                let fid = self.walk_from_source(&client, &path, self.lookup_timeout())?;
                let stat = client.stat_timeout(fid, self.lookup_timeout())?;
                let old_fid = self.nodes()?.replace_binding(nodeid, fid, stat)?;
                if self.config.debug {
                    eprintln!("r9p mount: node {nodeid} rebound to fid {fid}");
                }
                if let Some(old_fid) = old_fid {
                    let _ = client.clunk_timeout(old_fid, self.control_timeout());
                }
                self.status.set_data_session("connected", None);
                Ok((client, fid))
            }
        }
    }

    pub(in crate::fuse) fn fresh_node_fid(
        &self,
        nodeid: u64,
        client: &Client,
        timeout: Duration,
    ) -> Result<r9p::fid::Fid> {
        let path = {
            let nodes = self.nodes()?;
            nodes.node(nodeid)?.path.clone()
        };
        self.walk_from_source(client, &path, timeout)
    }

    pub(in crate::fuse) fn fresh_child_fid(
        &self,
        parent_nodeid: u64,
        name: &[u8],
        client: &Client,
        timeout: Duration,
    ) -> Result<r9p::fid::Fid> {
        let path = {
            let nodes = self.nodes()?;
            nodes.child_path(parent_nodeid, name)?
        };
        self.walk_from_source(client, &path, timeout)
    }

    pub(in crate::fuse) fn cached_node_stat_if_fresh(&self, nodeid: u64) -> Result<Option<Stat>> {
        let nodes = self.nodes()?;
        let node = nodes.node(nodeid)?;
        if node.needs_rebind || self.config.attr_timeout.is_zero() {
            return Ok(None);
        }
        if !node
            .stat_freshness
            .is_fresh_at(Instant::now(), self.config.attr_timeout)
        {
            return Ok(None);
        }
        Ok(Some(node.stat.clone()))
    }

    pub(in crate::fuse) fn invalidate_namespace_bindings_after_reply(
        &self,
        file: &mut File,
        reason: &'static str,
    ) {
        let stale_bindings = match self.nodes() {
            Ok(mut nodes) => nodes.mark_path_bindings_stale(),
            Err(error) => {
                self.record_mount_diagnostic(
                    "namespace_binding_invalidation_failed",
                    error.errno,
                    error.message(),
                );
                return;
            }
        };
        let invalidation = KernelInvalidation::coarse(stale_bindings);
        notify_kernel_invalidations(file, &invalidation);
        self.clunk_stale_bindings(invalidation.stale_bindings);
        self.record_mount_diagnostic("namespace_bindings_invalidated", 0, reason);
    }

    pub(in crate::fuse) fn clunk_stale_bindings(&self, stale: Vec<StaleBinding>) {
        if stale.is_empty() {
            return;
        }
        if let Ok(client) = self.client_snapshot() {
            let timeout = self.control_timeout();
            thread::spawn(move || {
                for binding in stale {
                    if let Some(fid) = binding.fid {
                        let _ = client.clunk_timeout(fid, timeout);
                    }
                }
            });
        }
    }

    pub(in crate::fuse) fn client_snapshot(&self) -> Result<Client> {
        Ok(self.client.snapshot()?)
    }

    pub(in crate::fuse) fn lookup_timeout(&self) -> Duration {
        self.config.lookup_timeout
    }

    pub(in crate::fuse) fn read_timeout(&self) -> Duration {
        self.config.read_timeout
    }

    pub(in crate::fuse) fn write_timeout(&self) -> Duration {
        self.config.write_timeout
    }

    pub(in crate::fuse) fn mutation_timeout(&self) -> Duration {
        self.config.mutation_timeout
    }

    pub(in crate::fuse) fn control_timeout(&self) -> Duration {
        self.config.control_timeout
    }

    pub(in crate::fuse) fn interrupt_timeout(&self) -> Duration {
        self.config.interrupt_timeout
    }

    pub(in crate::fuse) fn record_diagnostic(
        &self,
        event: &'static str,
        header: wire::FuseInHeader,
        errno: i32,
        message: impl Into<String>,
    ) {
        let context = self.diagnostic_context(header, &[]);
        self.record_diagnostic_with_context(event, header, errno, message, context);
    }

    pub(in crate::fuse) fn record_diagnostic_with_context(
        &self,
        event: &'static str,
        header: wire::FuseInHeader,
        errno: i32,
        message: impl Into<String>,
        context: DiagnosticContext,
    ) {
        let _ = self.diagnostics.record_entry(DiagnosticRecord {
            event,
            opcode: header.opcode,
            unique: header.unique,
            nodeid: header.nodeid,
            errno,
            message: message.into(),
            context,
        });
    }

    pub(in crate::fuse) fn record_mount_diagnostic(
        &self,
        event: &'static str,
        errno: i32,
        message: impl Into<String>,
    ) {
        let _ = self.diagnostics.record(event, 0, 0, 0, errno, message);
    }

    pub(in crate::fuse) fn diagnostic_context(
        &self,
        header: wire::FuseInHeader,
        payload: &[u8],
    ) -> DiagnosticContext {
        let mut context = DiagnosticContext {
            path: self.path_for_nodeid(header.nodeid),
            ..DiagnosticContext::default()
        };
        match header.opcode {
            wire::FUSE_READ | wire::FUSE_READDIR | wire::FUSE_READDIRPLUS => {
                if let Ok(input) = reply::read_struct::<wire::FuseReadIn>(payload) {
                    context.fh = Some(input.fh);
                    context.offset = Some(input.offset);
                    context.size = Some(u64::from(input.size));
                }
            }
            wire::FUSE_WRITE => {
                if let Ok(input) = reply::read_struct::<wire::FuseWriteIn>(payload) {
                    context.fh = Some(input.fh);
                    context.offset = Some(input.offset);
                    context.size = Some(u64::from(input.size));
                }
            }
            wire::FUSE_RELEASE | wire::FUSE_RELEASEDIR => {
                if let Ok(input) = reply::read_struct::<wire::FuseReleaseIn>(payload) {
                    context.fh = Some(input.fh);
                }
            }
            wire::FUSE_FLUSH => {
                if let Ok(input) = reply::read_struct::<wire::FuseFlushIn>(payload) {
                    context.fh = Some(input.fh);
                }
            }
            wire::FUSE_FSYNC | wire::FUSE_FSYNCDIR => {
                if let Ok(input) = reply::read_struct::<wire::FuseFsyncIn>(payload) {
                    context.fh = Some(input.fh);
                }
            }
            wire::FUSE_SETATTR => {
                if let Ok(input) = reply::read_struct::<wire::FuseSetattrIn>(payload) {
                    if input.valid & wire::FATTR_FH != 0 {
                        context.fh = Some(input.fh);
                    }
                    if input.valid & wire::FATTR_SIZE != 0 {
                        context.size = Some(input.size);
                    }
                }
            }
            wire::FUSE_POLL => {
                if let Ok(input) = reply::read_struct::<wire::FusePollIn>(payload) {
                    context.fh = Some(input.fh);
                }
            }
            _ => {}
        }
        context
    }

    pub(in crate::fuse) fn attr(&self, stat: &Stat) -> FuseAttr {
        FuseAttr {
            ino: qid_to_inode(stat.qid),
            size: stat.length,
            blocks: stat.length.saturating_add(8191) / 8192,
            atime: u64::from(stat.atime),
            mtime: u64::from(stat.mtime),
            ctime: u64::from(stat.mtime),
            atimensec: 0,
            mtimensec: 0,
            ctimensec: 0,
            mode: mode_kind(stat) | (stat.mode & 0o777),
            nlink: 1,
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 8192,
            flags: 0,
        }
    }

    fn path_for_nodeid(&self, nodeid: u64) -> Option<String> {
        let nodes = self.nodes().ok()?;
        let node = nodes.node(nodeid).ok()?;
        Some(format_namespace_path(&node.path))
    }
}

fn format_namespace_path(path: &[Vec<u8>]) -> String {
    if path.is_empty() {
        return "/".to_string();
    }
    let mut out = String::new();
    for segment in path {
        out.push('/');
        out.push_str(&String::from_utf8_lossy(segment));
    }
    out
}
