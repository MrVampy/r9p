use super::*;
use crate::model::RenameRelayRequest;

impl FrontTree {
    pub(super) fn relay_rename_at(
        &mut self,
        olddirfid: Fid,
        olddir_qid: Qid,
        oldname: &[u8],
        newdirfid: Fid,
        newdir_qid: Qid,
        newname: &[u8],
    ) -> Result<()> {
        let old_binding = self
            .fids
            .get(&olddirfid)
            .cloned()
            .ok_or_else(|| Error::from_static(EBADFID))?;
        let new_binding = self
            .fids
            .get(&newdirfid)
            .cloned()
            .ok_or_else(|| Error::from_static(EBADFID))?;
        if old_binding.root != new_binding.root
            || old_binding.principal_id != new_binding.principal_id
            || old_binding.uname != new_binding.uname
            || old_binding.aname != new_binding.aname
        {
            return Err(Error::from_static(EPERM));
        }
        if old_binding.node == new_binding.node && oldname == newname {
            return Ok(());
        }

        let mut state = self.front.lock()?;
        if state.qid_for(old_binding.node)? != olddir_qid
            || state.qid_for(new_binding.node)? != newdir_qid
        {
            return Err(Error::from_static(EBADFID));
        }
        let old_parent_generation = match &state.node(old_binding.node)?.body {
            Body::Dir(directory) if directory.children.contains_key(oldname) => {
                state.node(old_binding.node)?.generation
            }
            Body::Dir(_) => return Err(Error::from_static(ENOENT)),
            _ => return Err(Error::from_static(ENOTDIR)),
        };
        let new_parent_generation = match &state.node(new_binding.node)?.body {
            Body::Dir(_) => state.node(new_binding.node)?.generation,
            _ => return Err(Error::from_static(ENOTDIR)),
        };
        let prefix = state
            .rename_relay_for(old_binding.node, new_binding.node)
            .ok_or_else(|| Error::from_static(EPERM))?;
        let old_parent = request_context(
            &old_binding,
            olddirfid,
            state.path_relative_to(old_binding.node, ROOT_ID)?,
            state.path_relative_to(old_binding.node, old_binding.root)?,
            RequestDetails {
                offset: 0,
                count: u32::try_from(oldname.len()).map_err(|_| Error::from_static(EPERM))?,
                open_mode: 0,
                pushed_generation: old_parent_generation,
                front_qid_path: Some(state.qid_for(old_binding.node)?.path),
            },
        );
        let new_parent = request_context(
            &new_binding,
            newdirfid,
            state.path_relative_to(new_binding.node, ROOT_ID)?,
            state.path_relative_to(new_binding.node, new_binding.root)?,
            RequestDetails {
                offset: 0,
                count: u32::try_from(newname.len()).map_err(|_| Error::from_static(EPERM))?,
                open_mode: 0,
                pushed_generation: new_parent_generation,
                front_qid_path: Some(state.qid_for(new_binding.node)?.path),
            },
        );
        let request_id = state.next_request_id;
        state.next_request_id = state.next_request_id.saturating_add(1);
        state
            .rename_relay_responses
            .insert(request_id, (prefix.clone(), None));
        state.rename_pending.push_back(RenameRelayRequest {
            request_id,
            prefix,
            old_parent,
            old_name: oldname.to_vec(),
            new_parent,
            new_name: newname.to_vec(),
        });
        drop(state);
        self.front.shared.1.notify_all();

        let state = self.front.lock()?;
        self.front.wait_rename_relay(state, request_id)
    }
}
