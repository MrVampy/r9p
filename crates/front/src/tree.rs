use crate::front::Front;
use crate::model::{
    created_child_path, open_allowed, Body, CreateRelayRequest, IntakeRequest, RequestContext,
    EBADFID, ENOENT, ENOTDIR, EPERM, ROOT_ID,
};
use crate::ReadTarget;
use r9p::error::{Error, Result};
use r9p::fid::Fid;
use r9p::qid::Qid;
use r9p::server::{FileTree, OpenFile, ReadData};
use r9p::stat::Stat;
use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;

pub struct FrontTree {
    front: Front,
    session_id: u64,
    fids: BTreeMap<Fid, FidBinding>,
    open_modes: BTreeMap<Fid, u8>,
    rpc_buffers: BTreeMap<Fid, RpcBuffer>,
    rpc_inflight: BTreeMap<Fid, u64>,
    write_relay_buffers: BTreeMap<Fid, WriteRelayBuffer>,
}

impl FrontTree {
    pub(crate) fn new(front: Front, session_id: u64) -> Self {
        Self {
            front,
            session_id,
            fids: BTreeMap::new(),
            open_modes: BTreeMap::new(),
            rpc_buffers: BTreeMap::new(),
            rpc_inflight: BTreeMap::new(),
            write_relay_buffers: BTreeMap::new(),
        }
    }
}

#[derive(Clone)]
struct FidBinding {
    node: u64,
    root: u64,
    session_id: u64,
    uname: Vec<u8>,
    aname: Vec<u8>,
    principal_id: String,
}

struct RpcBuffer {
    prefix: String,
    bytes: Vec<u8>,
    context: RequestContext,
}

struct WriteRelayBuffer {
    prefix: String,
    bytes: Vec<u8>,
    context: RequestContext,
}

fn request_context(
    binding: &FidBinding,
    fid: Fid,
    front_path: String,
    target_path: String,
    offset: u64,
    open_mode: u8,
    pushed_generation: u64,
) -> RequestContext {
    RequestContext {
        principal_id: binding.principal_id.clone(),
        uname: String::from_utf8_lossy(&binding.uname).into_owned(),
        aname: String::from_utf8_lossy(&binding.aname).into_owned(),
        session_id: binding.session_id,
        fid,
        front_path,
        target_path,
        offset,
        open_mode,
        pushed_generation,
    }
}

impl FileTree for FrontTree {
    fn attach(&mut self, fid: Fid, uname: &[u8], aname: &[u8]) -> Result<Qid> {
        let state = self.front.lock()?;
        let root = state.attach_root_for(uname, aname)?;
        let principal_id = state
            .principal_roots
            .get(uname)
            .map(|root| root.principal_id.clone())
            .unwrap_or_else(|| String::from_utf8_lossy(uname).into_owned());
        let qid = state.qid_for(root)?;
        self.fids.insert(
            fid,
            FidBinding {
                node: root,
                root,
                session_id: self.session_id,
                uname: uname.to_vec(),
                aname: aname.to_vec(),
                principal_id,
            },
        );
        Ok(qid)
    }

    fn walk(&mut self, fid: Fid, newfid: Fid, _start: Qid, names: &[Vec<u8>]) -> Result<Vec<Qid>> {
        let state = self.front.lock()?;
        let binding = self
            .fids
            .get(&fid)
            .cloned()
            .ok_or_else(|| Error::from_static(EBADFID))?;
        let mut current = binding.node;
        let mut qids = Vec::with_capacity(names.len());
        for name in names {
            let child = if name.as_slice() == b".." {
                if current == binding.root {
                    Some(current)
                } else {
                    Some(state.node(current)?.parent)
                }
            } else {
                match &state.node(current)?.body {
                    Body::Dir(children) => children.get(name.as_slice()).copied(),
                    _ => None,
                }
            };
            match child {
                Some(id) => {
                    qids.push(state.qid_for(id)?);
                    current = id;
                }
                None => break,
            }
        }
        if qids.len() == names.len() {
            self.fids.insert(
                newfid,
                FidBinding {
                    node: current,
                    ..binding
                },
            );
        }
        Ok(qids)
    }

    fn open(&mut self, fid: Fid, qid: Qid, mode: u8) -> Result<OpenFile> {
        let state = self.front.lock()?;
        let id = self
            .fids
            .get(&fid)
            .ok_or_else(|| Error::from_static(EBADFID))?
            .node;
        if state.qid_for(id)?.path != qid.path {
            return Err(Error::from_static(EBADFID));
        }
        if !open_allowed(state.node(id)?, mode) {
            return Err(Error::from_static(EPERM));
        }
        self.open_modes.insert(fid, mode);
        Ok(OpenFile {
            qid,
            iounit: state.protocol.iounit,
        })
    }

    fn create(&mut self, fid: Fid, qid: Qid, name: &[u8], perm: u32, mode: u8) -> Result<OpenFile> {
        let mut state = self.front.lock()?;
        let binding = self
            .fids
            .get(&fid)
            .cloned()
            .ok_or_else(|| Error::from_static(EBADFID))?;
        let parent_id = binding.node;
        if state.qid_for(parent_id)?.path != qid.path {
            return Err(Error::from_static(EBADFID));
        }
        if !matches!(state.node(parent_id)?.body, Body::Dir(_)) {
            return Err(Error::from_static(ENOTDIR));
        }
        let create_prefix = state.node(parent_id)?.create_relay.clone();
        if create_prefix.is_none() {
            if let Body::Dir(children) = &state.node(parent_id)?.body {
                if children.contains_key(name) {
                    return Err(Error::from_static("file exists"));
                }
            }
        }
        let create_prefix = create_prefix.ok_or_else(|| Error::from_static(EPERM))?;
        let name_text = std::str::from_utf8(name)
            .map_err(|_| Error::from_static("create name is not utf-8"))?
            .to_string();
        let parent_path = state.path_relative_to(parent_id, binding.root)?;
        let target_path = created_child_path(&parent_path, &name_text);
        let parent_front_path = state.path_relative_to(parent_id, ROOT_ID)?;
        let front_path = created_child_path(&parent_front_path, &name_text);
        let parent_generation = state.node(parent_id)?.generation;
        let request_context = request_context(
            &binding,
            fid,
            front_path,
            target_path,
            0,
            mode,
            parent_generation,
        );
        let request_id = state.next_request_id;
        state.next_request_id = state.next_request_id.saturating_add(1);
        state.create_relay_responses.insert(request_id, None);
        state.create_pending.push_back(CreateRelayRequest {
            request_id,
            prefix: create_prefix.clone(),
            name: name_text.clone(),
            perm,
            mode,
            context: request_context,
        });
        drop(state);
        self.front.shared.1.notify_all();

        let state = self.front.lock()?;
        let (qtype, qid_version, qid_path) = self.front.wait_create_relay(state, request_id)?;
        let mut state = self.front.lock()?;
        let id = state.insert_created_relay_node(
            parent_id,
            &name_text,
            Qid::new(qtype, qid_version, qid_path),
            parent_generation,
            create_prefix,
        )?;
        self.fids.insert(
            fid,
            FidBinding {
                node: id,
                ..binding
            },
        );
        self.open_modes.insert(fid, mode);
        Ok(OpenFile {
            qid: Qid::new(qtype, qid_version, qid_path),
            iounit: state.protocol.iounit,
        })
    }

    fn read(&mut self, fid: Fid, _qid: Qid, offset: u64, count: u32) -> Result<ReadData> {
        match self.read_target(fid)? {
            ReadTarget::Node(id) => self.front.read_node(id, offset, count, None),
            ReadTarget::Rpc(request_id) => self.front.rpc_read(request_id, offset, count, None),
        }
    }

    fn write(&mut self, fid: Fid, _qid: Qid, offset: u64, data: &[u8]) -> Result<u32> {
        let mut state = self.front.lock()?;
        let binding = self
            .fids
            .get(&fid)
            .cloned()
            .ok_or_else(|| Error::from_static(EBADFID))?;
        let id = binding.node;
        let open_mode = self.open_modes.get(&fid).copied().unwrap_or(0);
        let target_path = state.path_relative_to(id, binding.root)?;
        let front_path = state.path_relative_to(id, ROOT_ID)?;
        let pushed_generation = state.node(id)?.generation;
        let request_context = request_context(
            &binding,
            fid,
            front_path,
            target_path,
            offset,
            open_mode,
            pushed_generation,
        );
        if let Body::Rpc(prefix) = &state.node(id)?.body {
            let prefix = prefix.clone();
            if let Some(previous) = self.rpc_inflight.get(&fid).copied() {
                if offset != 0 {
                    return Err(Error::from_static("rpc request already submitted"));
                }
                self.rpc_inflight.remove(&fid);
                state.rpc_responses.remove(&previous);
            }
            let offset = usize::try_from(offset).map_err(|_| Error::from_static(EPERM))?;
            if offset == 0 {
                self.rpc_buffers.insert(
                    fid,
                    RpcBuffer {
                        prefix,
                        bytes: data.to_vec(),
                        context: request_context,
                    },
                );
            } else {
                let buffer = self
                    .rpc_buffers
                    .get_mut(&fid)
                    .ok_or_else(|| Error::from_static("rpc write offset without request start"))?;
                if buffer.prefix != prefix {
                    return Err(Error::from_static("rpc path changed while buffering"));
                }
                if offset != buffer.bytes.len() {
                    return Err(Error::from_static("rpc write offset is not sequential"));
                }
                buffer.bytes.extend_from_slice(data);
            }
            return u32::try_from(data.len()).map_err(|_| Error::from_static(EPERM));
        }
        let write_relay = state.node(id)?.write_relay.clone().or_else(|| {
            if let Body::WriteRelay(prefix) = &state.node(id).ok()?.body {
                Some(prefix.clone())
            } else {
                None
            }
        });
        if let Some(prefix) = write_relay {
            if let Some(buffer) = self.write_relay_buffers.get_mut(&fid) {
                if buffer.prefix != prefix {
                    return Err(Error::from_static(
                        "write relay path changed while buffering",
                    ));
                }
                let expected = buffer
                    .context
                    .offset
                    .checked_add(
                        u64::try_from(buffer.bytes.len()).map_err(|_| Error::from_static(EPERM))?,
                    )
                    .ok_or_else(|| Error::from_static(EPERM))?;
                if offset != expected {
                    return Err(Error::from_static("write relay offset is not sequential"));
                }
                buffer.bytes.extend_from_slice(data);
            } else {
                self.write_relay_buffers.insert(
                    fid,
                    WriteRelayBuffer {
                        prefix,
                        bytes: data.to_vec(),
                        context: request_context,
                    },
                );
            }
            return u32::try_from(data.len()).map_err(|_| Error::from_static(EPERM));
        }
        let intake_id = match state.node(id)?.body {
            Body::IntakeNew(intake_id) => intake_id,
            _ => return Err(Error::from_static(EPERM)),
        };
        let prefix = state
            .intakes
            .get(&intake_id)
            .ok_or_else(|| Error::from_static(ENOENT))?
            .prefix
            .clone();
        let request_id = state.next_request_id;
        state.next_request_id = state.next_request_id.saturating_add(1);
        state.place(
            &format!("{prefix}/{request_id}/request"),
            Body::File(data.to_vec()),
        )?;
        state.place(
            &format!("{prefix}/created"),
            Body::File(request_id.to_string().into_bytes()),
        )?;
        state.pending.push_back(IntakeRequest {
            request_id,
            prefix,
            bytes: data.to_vec(),
            context: request_context,
        });
        drop(state);
        self.front.shared.1.notify_all();
        u32::try_from(data.len()).map_err(|_| Error::from_static(EPERM))
    }

    fn stat(&mut self, qid: Qid) -> Result<Stat> {
        let state = self.front.lock()?;
        let id = state.node_id_for_qid_path(qid.path)?;
        state.stat_for(id)
    }

    fn clunk(&mut self, fid: Fid, _qid: Qid) -> Result<()> {
        let write_relay_result = self
            .write_relay_buffers
            .remove(&fid)
            .map(|buffer| self.commit_write_relay(buffer))
            .unwrap_or(Ok(()));
        self.fids.remove(&fid);
        self.open_modes.remove(&fid);
        self.rpc_buffers.remove(&fid);
        if let Some(request_id) = self.rpc_inflight.remove(&fid) {
            if let Ok(mut state) = self.front.lock() {
                state.rpc_responses.remove(&request_id);
                drop(state);
                self.front.shared.1.notify_all();
            }
        }
        write_relay_result
    }

    fn remove(&mut self, fid: Fid, qid: Qid) -> Result<()> {
        let mut state = self.front.lock()?;
        let binding = self
            .fids
            .get(&fid)
            .cloned()
            .ok_or_else(|| Error::from_static(EBADFID))?;
        let id = binding.node;
        if state.qid_for(id)?.path != qid.path {
            return Err(Error::from_static(EBADFID));
        }
        let prefix = state
            .node(id)?
            .remove_relay
            .clone()
            .ok_or_else(|| Error::from_static(EPERM))?;
        let target_path = state.path_relative_to(id, binding.root)?;
        let front_path = state.path_relative_to(id, ROOT_ID)?;
        let pushed_generation = state.node(id)?.generation;
        let request_context = request_context(
            &binding,
            fid,
            front_path.clone(),
            target_path,
            0,
            0,
            pushed_generation,
        );
        let request_id = state.next_request_id;
        state.next_request_id = state.next_request_id.saturating_add(1);
        state.remove_relay_responses.insert(request_id, None);
        state.pending.push_back(IntakeRequest {
            request_id,
            prefix,
            bytes: Vec::new(),
            context: request_context,
        });
        drop(state);
        self.front.shared.1.notify_all();

        let state = self.front.lock()?;
        self.front.wait_remove_relay(state, request_id)?;
        let mut state = self.front.lock()?;
        state.remove_subtree_if_exists(&front_path)?;
        self.fids.remove(&fid);
        self.open_modes.remove(&fid);
        self.rpc_buffers.remove(&fid);
        self.write_relay_buffers.remove(&fid);
        if let Some(request_id) = self.rpc_inflight.remove(&fid) {
            state.rpc_responses.remove(&request_id);
        }
        Ok(())
    }

    fn wstat(&mut self, fid: Fid, qid: Qid, stat: &Stat) -> Result<()> {
        let mut state = self.front.lock()?;
        let binding = self
            .fids
            .get(&fid)
            .cloned()
            .ok_or_else(|| Error::from_static(EBADFID))?;
        let id = binding.node;
        if state.qid_for(id)?.path != qid.path {
            return Err(Error::from_static(EBADFID));
        }
        let prefix = state
            .node(id)?
            .wstat_relay
            .clone()
            .ok_or_else(|| Error::from_static(EPERM))?;
        let target_path = state.path_relative_to(id, binding.root)?;
        let front_path = state.path_relative_to(id, ROOT_ID)?;
        let open_mode = self.open_modes.get(&fid).copied().unwrap_or(0);
        let pushed_generation = state.node(id)?.generation;
        let request_context = request_context(
            &binding,
            fid,
            front_path,
            target_path,
            0,
            open_mode,
            pushed_generation,
        );
        let request_id = state.next_request_id;
        state.next_request_id = state.next_request_id.saturating_add(1);
        state.wstat_relay_responses.insert(request_id, None);
        state.pending.push_back(IntakeRequest {
            request_id,
            prefix,
            bytes: stat.encode()?,
            context: request_context,
        });
        drop(state);
        self.front.shared.1.notify_all();

        let state = self.front.lock()?;
        self.front.wait_wstat_relay(state, request_id)
    }
}

impl FrontTree {
    pub fn read_with_cancel(
        &mut self,
        fid: Fid,
        offset: u64,
        count: u32,
        cancel: Option<&AtomicBool>,
    ) -> Result<ReadData> {
        match self.read_target(fid)? {
            ReadTarget::Node(id) => self.front.read_node(id, offset, count, cancel),
            ReadTarget::Rpc(request_id) => self.front.rpc_read(request_id, offset, count, cancel),
        }
    }

    pub(crate) fn read_target(&mut self, fid: Fid) -> Result<ReadTarget> {
        let id = self
            .fids
            .get(&fid)
            .ok_or_else(|| Error::from_static(EBADFID))?
            .node;
        let mut state = self.front.lock()?;
        if matches!(state.node(id)?.body, Body::Rpc(_)) {
            let request_id = match self.rpc_inflight.get(&fid).copied() {
                Some(request_id) => request_id,
                None => {
                    let buffer = self
                        .rpc_buffers
                        .remove(&fid)
                        .ok_or_else(|| Error::from_static("rpc read before write on this fid"))?;
                    let request_id = state.next_request_id;
                    state.next_request_id = state.next_request_id.saturating_add(1);
                    state.rpc_responses.insert(request_id, None);
                    state.pending.push_back(IntakeRequest {
                        request_id,
                        prefix: buffer.prefix,
                        bytes: buffer.bytes,
                        context: buffer.context,
                    });
                    self.rpc_inflight.insert(fid, request_id);
                    drop(state);
                    self.front.shared.1.notify_all();
                    request_id
                }
            };
            return Ok(ReadTarget::Rpc(request_id));
        }
        Ok(ReadTarget::Node(id))
    }

    pub(crate) fn front(&self) -> Front {
        self.front.clone()
    }

    fn commit_write_relay(&self, buffer: WriteRelayBuffer) -> Result<()> {
        let mut state = self.front.lock()?;
        let request_id = state.next_request_id;
        state.next_request_id = state.next_request_id.saturating_add(1);
        state.write_relay_responses.insert(request_id, None);
        let data_len = buffer.bytes.len();
        state.pending.push_back(IntakeRequest {
            request_id,
            prefix: buffer.prefix,
            bytes: buffer.bytes,
            context: buffer.context,
        });
        drop(state);
        self.front.shared.1.notify_all();

        let state = self.front.lock()?;
        self.front
            .wait_write_relay(state, request_id, data_len, None)
            .map(|_| ())
    }
}

impl Drop for FrontTree {
    fn drop(&mut self) {
        if self.rpc_inflight.is_empty() {
            self.rpc_buffers.clear();
            self.write_relay_buffers.clear();
            return;
        }
        if let Ok(mut state) = self.front.lock() {
            for (_, request_id) in std::mem::take(&mut self.rpc_inflight) {
                state.rpc_responses.remove(&request_id);
            }
            self.rpc_buffers.clear();
            self.write_relay_buffers.clear();
            drop(state);
            self.front.shared.1.notify_all();
        }
    }
}
