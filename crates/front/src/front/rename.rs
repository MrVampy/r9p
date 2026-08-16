use super::*;
use crate::model::{RenameRelayReply, RenameRelayRequest};

impl Front {
    pub fn register_rename_relay(&self, path: &str, request_prefix: &str) -> Result<()> {
        let request_prefix = normalise_request_prefix(request_prefix)?;
        let mut state = self.lock()?;
        let trimmed = path.trim_matches('/');
        let root = if trimmed.is_empty() {
            crate::model::ROOT_ID
        } else {
            state.ensure_path_dir(trimmed)?
        };
        if !matches!(state.node(root)?.body, Body::Dir(_)) {
            return Err(Error::from_static(ENOTDIR));
        }
        state.rename_relay_roots.insert(root, request_prefix);
        Ok(())
    }

    pub fn next_rename_request_for_prefix(
        &self,
        prefix: &str,
        timeout: Duration,
    ) -> Result<Option<RenameRelayRequest>> {
        let prefix = normalise_request_prefix(prefix)?;
        let deadline = Instant::now() + timeout;
        let mut state = self.lock()?;
        loop {
            if let Some(request) = state.pop_rename_pending_for_prefix(&prefix) {
                return Ok(Some(request));
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            let (next, _timeout_result) = self
                .shared
                .1
                .wait_timeout(state, deadline - now)
                .map_err(|_| Error::from_static("front state poisoned"))?;
            state = next;
        }
    }

    pub fn next_rename_request_for_prefix_blocking(
        &self,
        prefix: &str,
    ) -> Result<RenameRelayRequest> {
        let prefix = normalise_request_prefix(prefix)?;
        let mut state = self.lock()?;
        loop {
            if let Some(request) = state.pop_rename_pending_for_prefix(&prefix) {
                return Ok(request);
            }
            state = self
                .shared
                .1
                .wait(state)
                .map_err(|_| Error::from_static("front state poisoned"))?;
        }
    }

    /// Accepts an owner-atomic rename after the owner has committed its state
    /// and synchronously projected the resulting namespace shape into Front.
    pub fn complete_rename(&self, prefix: &str, request_id: u64) -> Result<()> {
        self.complete_rename_result(prefix, request_id, RenameRelayReply::Accepted)
    }

    pub fn reject_rename(&self, prefix: &str, request_id: u64, message: &str) -> Result<()> {
        self.complete_rename_result(
            prefix,
            request_id,
            RenameRelayReply::Rejected(message.to_string()),
        )
    }

    fn complete_rename_result(
        &self,
        prefix: &str,
        request_id: u64,
        reply: RenameRelayReply,
    ) -> Result<()> {
        let prefix = normalise_request_prefix(prefix)?;
        let mut state = self.lock()?;
        let (request_prefix, slot) = state
            .rename_relay_responses
            .get_mut(&request_id)
            .ok_or_else(|| Error::from_static(ENOENT))?;
        if request_prefix != &prefix {
            return Err(Error::from_static(ENOENT));
        }
        if slot.is_some() {
            return Err(Error::from_static("rename relay already completed"));
        }
        *slot = Some(reply);
        drop(state);
        self.shared.1.notify_all();
        Ok(())
    }

    pub(crate) fn wait_rename_relay(
        &self,
        mut state: std::sync::MutexGuard<'_, State>,
        request_id: u64,
    ) -> Result<()> {
        let deadline = Instant::now() + state.wait_timeout;
        loop {
            match state.rename_relay_responses.remove(&request_id) {
                None => return Err(Error::from_static(ENOENT)),
                Some((_prefix, Some(RenameRelayReply::Accepted))) => return Ok(()),
                Some((_prefix, Some(RenameRelayReply::Rejected(message)))) => {
                    return Err(Error::from(message));
                }
                Some((prefix, None)) => {
                    state
                        .rename_relay_responses
                        .insert(request_id, (prefix, None));
                }
            }
            let now = Instant::now();
            if now >= deadline {
                state.rename_relay_responses.remove(&request_id);
                state
                    .rename_pending
                    .retain(|request| request.request_id != request_id);
                return Err(Error::from_static("rename relay unavailable"));
            }
            let (next, _timeout_result) = self
                .shared
                .1
                .wait_timeout(state, deadline - now)
                .map_err(|_| Error::from_static("front state poisoned"))?;
            state = next;
        }
    }
}
