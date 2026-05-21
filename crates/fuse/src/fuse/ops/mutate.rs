//! `unlink` / `rmdir` / `rename` op handlers.
//!
//! Rename in particular emulates file-over-file overwrite so editor temp-file
//! save flows work over 9P, where the wire protocol has no native rename atom.

use crate::{
    error::{Error, Result},
    fuse::{
        reply::{c_string, next_c_string, read_struct, reply_empty, reply_error},
        util::{is_namespace_shape_error, is_transport_error},
        wire::{FuseInHeader, FuseRenameIn},
        R9pFuse,
    },
    node::{is_dir, null_wstat},
    p9::Client,
};
use r9p::{fid::Fid, qid::Qid, stat::Stat};
use std::{fs::File, mem::size_of};

struct RenamePlan {
    client: Client,
    parent_fid: Fid,
    fid: Fid,
    before: Stat,
    old_path: Vec<Vec<u8>>,
    replaced_qid: Option<Qid>,
    new_path: Vec<Vec<u8>>,
}

impl R9pFuse {
    pub(in crate::fuse) fn remove(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
        payload: &[u8],
        _is_dir_remove: bool,
    ) -> Result<()> {
        let name = c_string(payload)?;
        let removed_path = self.nodes()?.child_path(header.nodeid, name)?;
        let (client, fid) = match self.walk_child_for_mutation(header.nodeid, name) {
            Ok(walked) => walked,
            Err(error) if is_transport_error(&error) => {
                self.reconnect()?;
                self.walk_child_for_mutation(header.nodeid, name)?
            }
            Err(error) if is_namespace_shape_error(&error) => {
                self.refresh_node(header.nodeid)?;
                self.walk_child_for_mutation(header.nodeid, name)?
            }
            Err(error) => return Err(error),
        };
        client.remove_timeout(fid, self.mutation_timeout())?;
        let stale_fids = self.nodes()?.remove_path_subtree(&removed_path);
        for stale_fid in stale_fids {
            let _ = client.clunk_timeout(stale_fid, self.control_timeout());
        }
        reply_empty(file, header.unique)
    }

    pub(in crate::fuse) fn rename(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
        payload: &[u8],
    ) -> Result<()> {
        let input = read_struct::<FuseRenameIn>(payload)?;
        if input.newdir != header.nodeid {
            return reply_error(file, header.unique, libc::EXDEV);
        }
        let names = payload
            .get(size_of::<FuseRenameIn>()..)
            .ok_or_else(|| Error::new(libc::EINVAL, "missing rename names"))?;
        let (old_name, rest) = next_c_string(names)?;
        let (new_name, _rest) = next_c_string(rest)?;
        let plan = match self.prepare_rename(header.nodeid, old_name, new_name) {
            Ok(plan) => plan,
            Err(error) if is_transport_error(&error) => {
                self.reconnect()?;
                self.prepare_rename(header.nodeid, old_name, new_name)?
            }
            Err(error) if is_namespace_shape_error(&error) => {
                self.refresh_node(header.nodeid)?;
                self.prepare_rename(header.nodeid, old_name, new_name)?
            }
            Err(error) => return Err(error),
        };
        self.rename_prepared(file, header.unique, new_name, plan)
    }

    fn walk_child_for_mutation(
        &mut self,
        parent_nodeid: u64,
        name: &[u8],
    ) -> Result<(Client, Fid)> {
        let (client, parent_fid) = self.bound_node_fid(parent_nodeid)?;
        let fid = client.walk_one_timeout(parent_fid, name, self.mutation_timeout())?;
        Ok((client, fid))
    }

    fn prepare_rename(
        &mut self,
        parent_nodeid: u64,
        old_name: &[u8],
        new_name: &[u8],
    ) -> Result<RenamePlan> {
        let old_path = self.nodes()?.child_path(parent_nodeid, old_name)?;
        let new_path = self.nodes()?.child_path(parent_nodeid, new_name)?;
        let (client, parent_fid) = self.bound_node_fid(parent_nodeid)?;
        let fid = client.walk_one_timeout(parent_fid, old_name, self.mutation_timeout())?;
        let before = client.stat_timeout(fid, self.lookup_timeout())?;
        let mut replaced_qid = None;
        if let Ok(existing) = client.walk_one_timeout(parent_fid, new_name, self.lookup_timeout()) {
            let existing_stat = match client.stat_timeout(existing, self.lookup_timeout()) {
                Ok(stat) => stat,
                Err(error) => {
                    let _ = client.clunk_timeout(existing, self.control_timeout());
                    let _ = client.clunk_timeout(fid, self.control_timeout());
                    return Err(error);
                }
            };
            if is_dir(&existing_stat) {
                let _ = client.clunk_timeout(existing, self.control_timeout());
                let _ = client.clunk_timeout(fid, self.control_timeout());
                return Err(Error::new(
                    libc::EISDIR,
                    "cannot rename a file over a directory",
                ));
            }
            replaced_qid = Some(existing_stat.qid);
            let _ = client.clunk_timeout(existing, self.control_timeout());
        }
        Ok(RenamePlan {
            client,
            parent_fid,
            fid,
            before,
            old_path,
            replaced_qid,
            new_path,
        })
    }

    fn rename_prepared(
        &mut self,
        file: &mut File,
        unique: u64,
        new_name: &[u8],
        plan: RenamePlan,
    ) -> Result<()> {
        let RenamePlan {
            client,
            parent_fid,
            fid,
            before,
            old_path,
            mut replaced_qid,
            new_path,
        } = plan;
        if let Err(error) = self.rename_fid(&client, fid, new_name) {
            if error.errno == libc::EEXIST {
                if let Ok(existing) =
                    client.walk_one_timeout(parent_fid, new_name, self.lookup_timeout())
                {
                    let existing_stat = match client.stat_timeout(existing, self.lookup_timeout()) {
                        Ok(stat) => stat,
                        Err(error) => {
                            let _ = client.clunk_timeout(existing, self.control_timeout());
                            let _ = client.clunk_timeout(fid, self.control_timeout());
                            return Err(error);
                        }
                    };
                    if is_dir(&existing_stat) {
                        let _ = client.clunk_timeout(existing, self.control_timeout());
                        let _ = client.clunk_timeout(fid, self.control_timeout());
                        return Err(Error::new(
                            libc::EISDIR,
                            "cannot rename a file over a directory",
                        ));
                    }
                    replaced_qid = Some(existing_stat.qid);
                    let _ = client.remove_timeout(existing, self.mutation_timeout());
                }
                self.rename_fid(&client, fid, new_name)?;
            } else {
                let _ = client.clunk_timeout(fid, self.control_timeout());
                return Err(error);
            }
        }
        let (fid, after) = self.stat_renamed_fid(&client, parent_fid, fid, new_name)?;
        self.nodes()?.move_path_prefix(&old_path, &new_path);
        let source_rebound = match self.nodes()?.replace_first_qid(
            before.qid,
            fid,
            after.clone(),
            Some(new_path.clone()),
        ) {
            Some(old_fid) => {
                let _ = client.clunk_timeout(old_fid, self.control_timeout());
                true
            }
            None => false,
        };
        if let Some(qid) = replaced_qid {
            if let Ok(replacement) =
                client.walk_one_timeout(parent_fid, new_name, self.lookup_timeout())
            {
                if let Some(old_fid) = self.nodes()?.replace_first_qid(
                    qid,
                    replacement,
                    after.clone(),
                    Some(new_path.clone()),
                ) {
                    let _ = client.clunk_timeout(old_fid, self.control_timeout());
                } else {
                    let _ = client.clunk_timeout(replacement, self.control_timeout());
                }
            }
        }
        if !source_rebound {
            self.nodes()?.refresh_qid(before.qid, after, Some(new_path));
            let _ = client.clunk_timeout(fid, self.control_timeout());
        }
        reply_empty(file, unique)
    }

    fn stat_renamed_fid(
        &self,
        client: &Client,
        parent_fid: Fid,
        fid: Fid,
        new_name: &[u8],
    ) -> Result<(Fid, Stat)> {
        match client.stat_timeout(fid, self.lookup_timeout()) {
            Ok(stat) => Ok((fid, stat)),
            Err(error) if is_namespace_shape_error(&error) => {
                let _ = client.clunk_timeout(fid, self.control_timeout());
                let rebound =
                    client.walk_one_timeout(parent_fid, new_name, self.lookup_timeout())?;
                match client.stat_timeout(rebound, self.lookup_timeout()) {
                    Ok(stat) => Ok((rebound, stat)),
                    Err(error) => {
                        let _ = client.clunk_timeout(rebound, self.control_timeout());
                        Err(error)
                    }
                }
            }
            Err(error) => {
                let _ = client.clunk_timeout(fid, self.control_timeout());
                Err(error)
            }
        }
    }

    fn rename_fid(&mut self, client: &Client, fid: Fid, new_name: &[u8]) -> Result<()> {
        let mut stat = null_wstat();
        stat.name = new_name.to_vec();
        client.wstat_timeout(fid, stat, self.mutation_timeout())
    }
}
