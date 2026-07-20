use crate::model::{
    canonical_root_path, normalise_request_prefix, Body, CreateRelayReply, CreateRelayRequest,
    Intake, IntakeRequest, LogBody, PrincipalRoot, ProtocolConfig, PushedDirectoryMetadata,
    PushedFileMetadata, RemoveRelayReply, RequestReply, State, WriteRelayReply, WstatRelayReply,
};
use crate::tree::FrontTree;
use r9p::codec::{MAX_MSIZE, MIN_MSIZE};
use r9p::error::{Error, Result, ENOENT, ENOTDIR, EPERM};
use r9p::server::ReadData;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct Front {
    pub(crate) shared: Arc<(Mutex<State>, Condvar)>,
    next_session_id: Arc<AtomicU64>,
}

impl Default for Front {
    fn default() -> Self {
        Self::new()
    }
}

impl Front {
    pub fn new() -> Self {
        Self {
            shared: Arc::new((Mutex::new(State::new()), Condvar::new())),
            next_session_id: Arc::new(AtomicU64::new(1)),
        }
    }

    pub(crate) fn lock(&self) -> Result<std::sync::MutexGuard<'_, State>> {
        self.shared
            .0
            .lock()
            .map_err(|_| Error::from_static("front state poisoned"))
    }

    pub fn set(&self, path: &str, bytes: &[u8]) -> Result<()> {
        self.lock()?.place(path, Body::File(bytes.to_vec()))?;
        self.shared.1.notify_all();
        Ok(())
    }

    pub fn set_pushed_file(
        &self,
        path: &str,
        bytes: &[u8],
        metadata: PushedFileMetadata,
    ) -> Result<()> {
        self.lock()?
            .place_pushed_file(path, bytes.to_vec(), metadata)?;
        self.shared.1.notify_all();
        Ok(())
    }

    pub fn set_pushed_directory(
        &self,
        path: &str,
        metadata: PushedDirectoryMetadata,
    ) -> Result<()> {
        self.lock()?.place_pushed_directory(path, metadata)?;
        self.shared.1.notify_all();
        Ok(())
    }

    pub fn remove_subtree_if_exists(&self, path: &str) -> Result<()> {
        self.lock()?.remove_subtree_if_exists(path)?;
        self.shared.1.notify_all();
        Ok(())
    }

    pub fn append_event(&self, path: &str, bytes: &[u8]) -> Result<()> {
        self.lock()?
            .place(path, Body::Log(LogBody::new(bytes.to_vec())))?;
        self.shared.1.notify_all();
        Ok(())
    }

    pub fn set_log_capacity(&self, capacity: usize) -> Result<()> {
        self.lock()?.log_capacity = capacity.max(1);
        Ok(())
    }

    pub fn set_wait_timeout(&self, timeout: Duration) -> Result<()> {
        self.lock()?.wait_timeout = timeout;
        Ok(())
    }

    pub fn set_protocol_limits(&self, max_msize: u32, iounit: u32) -> Result<()> {
        if !(MIN_MSIZE..=MAX_MSIZE).contains(&max_msize) {
            return Err(Error::from_static("invalid max msize"));
        }
        if iounit > max_msize {
            return Err(Error::from_static("invalid iounit"));
        }
        self.lock()?.protocol = ProtocolConfig { max_msize, iounit };
        Ok(())
    }

    pub fn register_intake(&self, prefix: &str) -> Result<()> {
        let mut state = self.lock()?;
        let trimmed = prefix.trim_matches('/').to_string();
        if trimmed.is_empty() {
            return Err(Error::from_static(EPERM));
        }
        let new_path = format!("{trimmed}/new");
        let id = state.place(&new_path, Body::IntakeNew(0))?;
        if let Some(node) = state.nodes.get_mut(&id) {
            node.body = Body::IntakeNew(id);
        }
        state.intakes.insert(id, Intake { prefix: trimmed });
        Ok(())
    }

    pub fn register_rpc(&self, path: &str) -> Result<()> {
        let mut state = self.lock()?;
        let trimmed = path.trim_matches('/');
        if trimmed.is_empty() {
            return Err(Error::from_static(EPERM));
        }
        state.place(trimmed, Body::Rpc(trimmed.to_string()))?;
        Ok(())
    }

    pub fn register_read_relay(&self, path: &str) -> Result<()> {
        let mut state = self.lock()?;
        let trimmed = normalise_request_prefix(path)?;
        match state.lookup_optional_path(&trimmed)? {
            Some(id) => {
                if !matches!(state.node(id)?.body, Body::ReadRelay(_)) {
                    return Err(Error::from_static(EPERM));
                }
                if let Some(node) = state.nodes.get_mut(&id) {
                    node.body = Body::ReadRelay(trimmed);
                }
            }
            None => {
                state.place(&trimmed, Body::ReadRelay(trimmed.clone()))?;
            }
        }
        Ok(())
    }

    pub fn register_write_relay(&self, path: &str) -> Result<()> {
        let mut state = self.lock()?;
        let trimmed = normalise_request_prefix(path)?;
        state.write_relay_prefixes.insert(trimmed.to_string());
        match state.lookup_optional_path(&trimmed)? {
            Some(id) => {
                if matches!(state.node(id)?.body, Body::Dir(_)) {
                    return Err(Error::from_static(EPERM));
                }
                if let Some(node) = state.nodes.get_mut(&id) {
                    if matches!(node.body, Body::WriteRelay(_)) {
                        node.body = Body::WriteRelay(trimmed.clone());
                    }
                    node.write_relay = Some(trimmed);
                }
            }
            None => {
                let id = state.place(&trimmed, Body::WriteRelay(trimmed.clone()))?;
                if let Some(node) = state.nodes.get_mut(&id) {
                    node.write_relay = Some(trimmed);
                }
            }
        }
        Ok(())
    }

    pub fn register_remove_relay(&self, path: &str) -> Result<()> {
        let mut state = self.lock()?;
        let trimmed = normalise_request_prefix(path)?;
        let id = state.lookup_path(&trimmed)?;
        state.remove_relay_prefixes.insert(trimmed.clone());
        if let Some(node) = state.nodes.get_mut(&id) {
            node.remove_relay = Some(trimmed);
        }
        Ok(())
    }

    pub fn register_wstat_relay(&self, path: &str) -> Result<()> {
        let mut state = self.lock()?;
        let trimmed = normalise_request_prefix(path)?;
        let id = state.lookup_path(&trimmed)?;
        state.wstat_relay_prefixes.insert(trimmed.clone());
        if let Some(node) = state.nodes.get_mut(&id) {
            node.wstat_relay = Some(trimmed);
        }
        Ok(())
    }

    pub fn register_create_relay(&self, path: &str) -> Result<()> {
        let mut state = self.lock()?;
        let trimmed = normalise_request_prefix(path)?;
        let id = state.ensure_path_dir(&trimmed)?;
        let node = state
            .nodes
            .get_mut(&id)
            .ok_or_else(|| Error::from_static(ENOENT))?;
        if !matches!(node.body, Body::Dir(_)) {
            return Err(Error::from_static(ENOTDIR));
        }
        node.create_relay = Some(trimmed.clone());
        state.write_relay_prefixes.insert(trimmed);
        Ok(())
    }

    pub fn register_log(&self, path: &str) -> Result<()> {
        let mut state = self.lock()?;
        let trimmed = path.trim_matches('/');
        if trimmed.is_empty() {
            return Err(Error::from_static(EPERM));
        }
        state.place(trimmed, Body::Log(LogBody::empty()))?;
        Ok(())
    }

    pub fn set_principal_root(&self, principal: &str, root_path: &str) -> Result<()> {
        self.set_principal_class_aname(principal, principal, "*", root_path)
    }

    pub fn set_principal_root_aname(
        &self,
        principal: &str,
        aname: &str,
        root_path: &str,
    ) -> Result<()> {
        self.set_principal_class_aname(principal, principal, aname, root_path)
    }

    pub fn set_principal_class_aname(
        &self,
        uname: &str,
        principal_id: &str,
        aname: &str,
        root_path: &str,
    ) -> Result<()> {
        if uname.is_empty() || principal_id.is_empty() {
            return Err(Error::from_static(EPERM));
        }
        if aname.is_empty() {
            return Err(Error::from_static(EPERM));
        }
        let mut state = self.lock()?;
        let root = state.lookup_path(root_path)?;
        if !matches!(state.node(root)?.body, Body::Dir(_)) {
            return Err(Error::from_static(ENOTDIR));
        }
        let root_path = canonical_root_path(root_path)?;
        state.principal_roots_required = true;
        match state.principal_roots.get_mut(uname.as_bytes()) {
            Some(existing) => {
                if existing.root_path != root_path {
                    return Err(Error::from_static("principal root path mismatch"));
                }
                if existing.principal_id != principal_id {
                    return Err(Error::from_static("principal id mismatch"));
                }
                existing.root = root;
                existing.anames.insert(aname.as_bytes().to_vec());
            }
            None => {
                let mut anames = BTreeSet::new();
                anames.insert(aname.as_bytes().to_vec());
                state.principal_roots.insert(
                    uname.as_bytes().to_vec(),
                    PrincipalRoot {
                        root,
                        root_path,
                        principal_id: principal_id.to_string(),
                        anames,
                    },
                );
            }
        }
        Ok(())
    }

    pub fn retain_principal_roots<'a, I>(&self, unames: I) -> Result<()>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let retain = unames
            .into_iter()
            .map(|uname| uname.as_bytes().to_vec())
            .collect::<BTreeSet<_>>();
        let mut state = self.lock()?;
        state
            .principal_roots
            .retain(|uname, _root| retain.contains(uname));
        Ok(())
    }

    pub fn next_request(&self, timeout: Duration) -> Result<Option<IntakeRequest>> {
        let deadline = Instant::now() + timeout;
        let mut state = self.lock()?;
        loop {
            if let Some(request) = state.pending.pop_front() {
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

    pub fn next_request_for_prefix(
        &self,
        prefix: &str,
        timeout: Duration,
    ) -> Result<Option<IntakeRequest>> {
        let prefix = normalise_request_prefix(prefix)?;
        let deadline = Instant::now() + timeout;
        let mut state = self.lock()?;
        loop {
            if let Some(request) = state.pop_pending_for_prefix(&prefix) {
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

    pub fn next_request_blocking(&self) -> Result<IntakeRequest> {
        let mut state = self.lock()?;
        loop {
            if let Some(request) = state.pending.pop_front() {
                return Ok(request);
            }
            state = self
                .shared
                .1
                .wait(state)
                .map_err(|_| Error::from_static("front state poisoned"))?;
        }
    }

    pub fn next_request_for_prefix_blocking(&self, prefix: &str) -> Result<IntakeRequest> {
        let prefix = normalise_request_prefix(prefix)?;
        let mut state = self.lock()?;
        loop {
            if let Some(request) = state.pop_pending_for_prefix(&prefix) {
                return Ok(request);
            }
            state = self
                .shared
                .1
                .wait(state)
                .map_err(|_| Error::from_static("front state poisoned"))?;
        }
    }

    pub fn next_create_request_for_prefix_blocking(
        &self,
        prefix: &str,
    ) -> Result<CreateRelayRequest> {
        let prefix = normalise_request_prefix(prefix)?;
        let mut state = self.lock()?;
        loop {
            if let Some(request) = state.pop_create_pending_for_prefix(&prefix) {
                return Ok(request);
            }
            state = self
                .shared
                .1
                .wait(state)
                .map_err(|_| Error::from_static("front state poisoned"))?;
        }
    }

    pub fn next_request_for_prefix_while_rpc_pending(
        &self,
        prefix: &str,
        rpc_request_id: u64,
    ) -> Result<Option<IntakeRequest>> {
        let prefix = normalise_request_prefix(prefix)?;
        let mut state = self.lock()?;
        loop {
            if !state.rpc_responses.contains_key(&rpc_request_id) {
                return Ok(None);
            }
            if let Some(request) = state.pop_pending_for_prefix(&prefix) {
                return Ok(Some(request));
            }
            state = self
                .shared
                .1
                .wait(state)
                .map_err(|_| Error::from_static("front state poisoned"))?;
        }
    }

    pub fn complete_request(&self, prefix: &str, request_id: u64, bytes: &[u8]) -> Result<()> {
        let trimmed = prefix.trim_matches('/');
        {
            let mut state = self.lock()?;
            if state.rpc_responses.contains_key(&request_id) {
                let trimmed = normalise_request_prefix(prefix)?;
                if state.response_prefixes.get(&request_id) != Some(&trimmed) {
                    return Err(Error::from_static(ENOENT));
                }
                let slot = state
                    .rpc_responses
                    .get_mut(&request_id)
                    .ok_or_else(|| Error::from_static(ENOENT))?;
                *slot = Some(RequestReply::Accepted(bytes.to_vec()));
                drop(state);
                self.shared.1.notify_all();
                return Ok(());
            }
            if !state.is_intake_prefix(trimmed) {
                return Err(Error::from_static(ENOENT));
            }
        }
        let result_path = format!("{trimmed}/{request_id}/result");
        self.set(&result_path, bytes)
    }

    pub fn reject_request(&self, prefix: &str, request_id: u64, message: &str) -> Result<()> {
        let mut state = self.lock()?;
        let trimmed = normalise_request_prefix(prefix)?;
        if state.response_prefixes.get(&request_id) != Some(&trimmed) {
            return Err(Error::from_static(ENOENT));
        }
        match state.rpc_responses.get_mut(&request_id) {
            Some(slot) => {
                *slot = Some(RequestReply::Rejected(message.to_string()));
                drop(state);
                self.shared.1.notify_all();
                Ok(())
            }
            None => Err(Error::from_static(ENOENT)),
        }
    }

    pub fn complete_write(&self, prefix: &str, request_id: u64, count: u32) -> Result<()> {
        self.complete_write_result(prefix, request_id, WriteRelayReply::Accepted(count))
    }

    pub fn reject_write(&self, prefix: &str, request_id: u64, message: &str) -> Result<()> {
        self.complete_write_result(
            prefix,
            request_id,
            WriteRelayReply::Rejected(message.to_string()),
        )
    }

    pub fn complete_remove(&self, prefix: &str, request_id: u64) -> Result<()> {
        self.complete_remove_result(prefix, request_id, RemoveRelayReply::Accepted)
    }

    pub fn reject_remove(&self, prefix: &str, request_id: u64, message: &str) -> Result<()> {
        self.complete_remove_result(
            prefix,
            request_id,
            RemoveRelayReply::Rejected(message.to_string()),
        )
    }

    pub fn complete_wstat(&self, prefix: &str, request_id: u64) -> Result<()> {
        self.complete_wstat_result(prefix, request_id, WstatRelayReply::Accepted)
    }

    pub fn reject_wstat(&self, prefix: &str, request_id: u64, message: &str) -> Result<()> {
        self.complete_wstat_result(
            prefix,
            request_id,
            WstatRelayReply::Rejected(message.to_string()),
        )
    }

    pub fn complete_create(
        &self,
        prefix: &str,
        request_id: u64,
        qtype: u8,
        qid_version: u32,
        qid_path: u64,
    ) -> Result<()> {
        self.complete_create_result(
            prefix,
            request_id,
            CreateRelayReply::Accepted {
                qtype,
                qid_version,
                qid_path,
            },
        )
    }

    pub fn reject_create(&self, prefix: &str, request_id: u64, message: &str) -> Result<()> {
        self.complete_create_result(
            prefix,
            request_id,
            CreateRelayReply::Rejected(message.to_string()),
        )
    }

    fn complete_create_result(
        &self,
        prefix: &str,
        request_id: u64,
        reply: CreateRelayReply,
    ) -> Result<()> {
        let mut state = self.lock()?;
        let prefix = normalise_request_prefix(prefix)?;
        if !state.write_relay_prefixes.contains(&prefix) {
            return Err(Error::from_static(ENOENT));
        }
        match state.create_relay_responses.get_mut(&request_id) {
            Some(slot) => {
                *slot = Some(reply);
                drop(state);
                self.shared.1.notify_all();
                Ok(())
            }
            None => Err(Error::from_static(ENOENT)),
        }
    }

    fn complete_write_result(
        &self,
        prefix: &str,
        request_id: u64,
        reply: WriteRelayReply,
    ) -> Result<()> {
        let mut state = self.lock()?;
        let prefix = normalise_request_prefix(prefix)?;
        if !state.write_relay_prefixes.contains(&prefix) {
            return Err(Error::from_static(ENOENT));
        }
        match state.write_relay_responses.get_mut(&request_id) {
            Some(slot) => {
                *slot = Some(reply);
                drop(state);
                self.shared.1.notify_all();
                Ok(())
            }
            None => Err(Error::from_static(ENOENT)),
        }
    }

    fn complete_remove_result(
        &self,
        prefix: &str,
        request_id: u64,
        reply: RemoveRelayReply,
    ) -> Result<()> {
        let mut state = self.lock()?;
        let prefix = normalise_request_prefix(prefix)?;
        if !state.remove_relay_prefixes.contains(&prefix) {
            return Err(Error::from_static(ENOENT));
        }
        match state.remove_relay_responses.get_mut(&request_id) {
            Some(slot) => {
                *slot = Some(reply);
                drop(state);
                self.shared.1.notify_all();
                Ok(())
            }
            None => Err(Error::from_static(ENOENT)),
        }
    }

    fn complete_wstat_result(
        &self,
        prefix: &str,
        request_id: u64,
        reply: WstatRelayReply,
    ) -> Result<()> {
        let mut state = self.lock()?;
        let prefix = normalise_request_prefix(prefix)?;
        if !state.wstat_relay_prefixes.contains(&prefix) {
            return Err(Error::from_static(ENOENT));
        }
        match state.wstat_relay_responses.get_mut(&request_id) {
            Some(slot) => {
                *slot = Some(reply);
                drop(state);
                self.shared.1.notify_all();
                Ok(())
            }
            None => Err(Error::from_static(ENOENT)),
        }
    }

    fn read_rpc(
        &self,
        mut state: std::sync::MutexGuard<'_, State>,
        request_id: u64,
        offset: u64,
        count: u32,
        cancel: Option<&AtomicBool>,
    ) -> Result<ReadData> {
        let deadline = Instant::now() + state.wait_timeout;
        loop {
            if cancel.is_some_and(|cancel| cancel.load(Ordering::SeqCst)) {
                state.remove_response_request(request_id);
                return Err(Error::from_static("request flushed"));
            }
            match state.rpc_responses.get(&request_id) {
                None => return Err(Error::from_static(ENOENT)),
                Some(Some(RequestReply::Accepted(bytes))) => {
                    let start = usize::try_from(offset.min(bytes.len() as u64))
                        .map_err(|_| Error::from_static(EPERM))?;
                    let end = bytes.len().min(start.saturating_add(count as usize));
                    return Ok(ReadData::Bytes(bytes[start..end].to_vec()));
                }
                Some(Some(RequestReply::Rejected(message))) => {
                    return Err(Error::from(message.clone()));
                }
                Some(None) => {}
            }
            let now = Instant::now();
            if now >= deadline {
                state.remove_response_request(request_id);
                return Err(Error::from_static(
                    "rpc request timed out awaiting response",
                ));
            }
            let (next, _timeout_result) = self
                .shared
                .1
                .wait_timeout(state, deadline - now)
                .map_err(|_| Error::from_static("front state poisoned"))?;
            state = next;
        }
    }

    pub(crate) fn response_read(
        &self,
        request_id: u64,
        offset: u64,
        count: u32,
        cancel: Option<&AtomicBool>,
        consume: bool,
    ) -> Result<ReadData> {
        let state = self.lock()?;
        let response = self.read_rpc(state, request_id, offset, count, cancel);
        if consume {
            let mut state = self.lock()?;
            state.remove_response_request(request_id);
        }
        response
    }

    pub(crate) fn wait_write_relay(
        &self,
        mut state: std::sync::MutexGuard<'_, State>,
        request_id: u64,
        data_len: usize,
        cancel: Option<&AtomicBool>,
    ) -> Result<u32> {
        let deadline = Instant::now() + state.wait_timeout;
        loop {
            if cancel.is_some_and(|cancel| cancel.load(Ordering::SeqCst)) {
                state.write_relay_responses.remove(&request_id);
                state.remove_pending_request(request_id);
                return Err(Error::from_static("request flushed"));
            }
            match state.write_relay_responses.remove(&request_id) {
                None => return Err(Error::from_static(ENOENT)),
                Some(Some(WriteRelayReply::Accepted(count))) => {
                    if usize::try_from(count).map_or(true, |count| count > data_len) {
                        return Err(Error::from_static(EPERM));
                    }
                    return Ok(count);
                }
                Some(Some(WriteRelayReply::Rejected(message))) => {
                    return Err(Error::from(message));
                }
                Some(None) => {
                    state.write_relay_responses.insert(request_id, None);
                }
            }
            let now = Instant::now();
            if now >= deadline {
                state.write_relay_responses.remove(&request_id);
                state.remove_pending_request(request_id);
                return Err(Error::from_static("write relay unavailable"));
            }
            let (next, _timeout_result) = self
                .shared
                .1
                .wait_timeout(state, deadline - now)
                .map_err(|_| Error::from_static("front state poisoned"))?;
            state = next;
        }
    }

    pub(crate) fn wait_remove_relay(
        &self,
        mut state: std::sync::MutexGuard<'_, State>,
        request_id: u64,
    ) -> Result<()> {
        let deadline = Instant::now() + state.wait_timeout;
        loop {
            match state.remove_relay_responses.remove(&request_id) {
                None => return Err(Error::from_static(ENOENT)),
                Some(Some(RemoveRelayReply::Accepted)) => return Ok(()),
                Some(Some(RemoveRelayReply::Rejected(message))) => {
                    return Err(Error::from(message));
                }
                Some(None) => {
                    state.remove_relay_responses.insert(request_id, None);
                }
            }
            let now = Instant::now();
            if now >= deadline {
                state.remove_relay_responses.remove(&request_id);
                state.remove_pending_request(request_id);
                return Err(Error::from_static("remove relay unavailable"));
            }
            let (next, _timeout_result) = self
                .shared
                .1
                .wait_timeout(state, deadline - now)
                .map_err(|_| Error::from_static("front state poisoned"))?;
            state = next;
        }
    }

    pub(crate) fn wait_wstat_relay(
        &self,
        mut state: std::sync::MutexGuard<'_, State>,
        request_id: u64,
    ) -> Result<()> {
        let deadline = Instant::now() + state.wait_timeout;
        loop {
            match state.wstat_relay_responses.remove(&request_id) {
                None => return Err(Error::from_static(ENOENT)),
                Some(Some(WstatRelayReply::Accepted)) => return Ok(()),
                Some(Some(WstatRelayReply::Rejected(message))) => {
                    return Err(Error::from(message));
                }
                Some(None) => {
                    state.wstat_relay_responses.insert(request_id, None);
                }
            }
            let now = Instant::now();
            if now >= deadline {
                state.wstat_relay_responses.remove(&request_id);
                state.remove_pending_request(request_id);
                return Err(Error::from_static("wstat relay unavailable"));
            }
            let (next, _timeout_result) = self
                .shared
                .1
                .wait_timeout(state, deadline - now)
                .map_err(|_| Error::from_static("front state poisoned"))?;
            state = next;
        }
    }

    pub(crate) fn wait_create_relay(
        &self,
        mut state: std::sync::MutexGuard<'_, State>,
        request_id: u64,
    ) -> Result<(u8, u32, u64)> {
        let deadline = Instant::now() + state.wait_timeout;
        loop {
            match state.create_relay_responses.remove(&request_id) {
                None => return Err(Error::from_static(ENOENT)),
                Some(Some(CreateRelayReply::Accepted {
                    qtype,
                    qid_version,
                    qid_path,
                })) => return Ok((qtype, qid_version, qid_path)),
                Some(Some(CreateRelayReply::Rejected(message))) => {
                    return Err(Error::from(message));
                }
                Some(None) => {
                    state.create_relay_responses.insert(request_id, None);
                }
            }
            let now = Instant::now();
            if now >= deadline {
                state.create_relay_responses.remove(&request_id);
                state
                    .create_pending
                    .retain(|request| request.request_id != request_id);
                return Err(Error::from_static("create relay unavailable"));
            }
            let (next, _timeout_result) = self
                .shared
                .1
                .wait_timeout(state, deadline - now)
                .map_err(|_| Error::from_static("front state poisoned"))?;
            state = next;
        }
    }

    fn read_log(
        &self,
        mut state: std::sync::MutexGuard<'_, State>,
        id: u64,
        offset: u64,
        count: u32,
        cancel: Option<&AtomicBool>,
    ) -> Result<ReadData> {
        let deadline = Instant::now() + state.wait_timeout;
        loop {
            if cancel.is_some_and(|cancel| cancel.load(Ordering::SeqCst)) {
                return Err(Error::from_static("request flushed"));
            }
            if let Body::Log(log) = &state.node(id)?.body {
                if offset < log.start {
                    return Err(Error::from(format!(
                        "log window passed: earliest retained offset {}",
                        log.start
                    )));
                }
                if offset < log.end() {
                    return Ok(ReadData::Bytes(log.read(offset, count as usize)));
                }
            } else {
                return Err(Error::from_static(ENOENT));
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(ReadData::Bytes(Vec::new()));
            }
            let (next, _timeout_result) = self
                .shared
                .1
                .wait_timeout(state, deadline - now)
                .map_err(|_| Error::from_static("front state poisoned"))?;
            state = next;
        }
    }

    pub fn tree(&self) -> FrontTree {
        FrontTree::new(
            self.clone(),
            self.next_session_id.fetch_add(1, Ordering::Relaxed),
        )
    }

    pub fn wake_readers(&self) {
        self.shared.1.notify_all();
    }

    pub(crate) fn max_msize(&self) -> Result<u32> {
        Ok(self.lock()?.protocol.max_msize)
    }

    pub(crate) fn read_node(
        &self,
        id: u64,
        offset: u64,
        count: u32,
        cancel: Option<&AtomicBool>,
    ) -> Result<ReadData> {
        let state = self.lock()?;
        match &state.node(id)?.body {
            Body::Dir(children) => {
                let mut stats = Vec::with_capacity(children.len());
                for &child in children.values() {
                    stats.push(state.stat_for(child)?);
                }
                Ok(ReadData::Directory(stats))
            }
            Body::File(bytes) => {
                let start = usize::try_from(offset.min(bytes.len() as u64))
                    .map_err(|_| Error::from_static(EPERM))?;
                let end = bytes.len().min(start.saturating_add(count as usize));
                Ok(ReadData::Bytes(bytes[start..end].to_vec()))
            }
            Body::Log(_) => self.read_log(state, id, offset, count, cancel),
            Body::IntakeNew(_) => Err(Error::from_static(EPERM)),
            Body::Rpc(_) => Err(Error::from_static(EPERM)),
            Body::ReadRelay(_) => Err(Error::from_static(EPERM)),
            Body::WriteRelay(_) => Err(Error::from_static(EPERM)),
        }
    }
}

pub(crate) enum ReadTarget {
    Node(u64),
    Response(u64, u64, bool),
}
