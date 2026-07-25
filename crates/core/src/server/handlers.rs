use crate::{
    codec::{clamp_read_count, max_iounit, max_write_payload},
    error::{
        Error, Result, EBADDIROFFSET, EBADMODE, EFIDNOTOPEN, EFIDOPEN, ENOAUTH, ENOENT, ENOTDIR,
        ESYMLINKDIALECT,
    },
    fid::{FidState, NOFID},
    flush::{FlushOutcome, RequestKey},
    message::{RMessage, TMessage, Tag},
    mode,
    qid::{Qid, DMDIR, DMSYMLINK},
    stat::{dirread_chunk, Stat},
};

use super::{
    session::VersionNegotiation,
    types::{ReadData, ServerCompletion, ServerEvent, ServerRequest, ServerRequestKind},
    validation::{error_reply, take_count, validate_walk_names, validate_wstat},
    FileTree, Server,
};

impl<T> Server<T> {
    pub fn admit(&mut self, message: TMessage) -> ServerEvent {
        if let TMessage::Version {
            tag,
            msize,
            version,
        } = message
        {
            return match self.session.reset_for_version(msize, &version) {
                Ok(VersionNegotiation::Accepted | VersionNegotiation::Unknown) => {
                    ServerEvent::Reply(RMessage::Version {
                        tag,
                        msize: self.session.msize(),
                        version: self.session.version().to_vec(),
                    })
                }
                Err(error) => ServerEvent::Reply(error_reply(tag, error)),
            };
        }

        if !self.session.is_negotiated() {
            return ServerEvent::Reply(error_reply(
                message.tag(),
                Error::from_static("version not negotiated"),
            ));
        }

        let tag = message.tag();
        let key = match self.session.requests.begin(tag) {
            Ok(key) => key,
            Err(error) => return ServerEvent::Reply(error_reply(tag, error)),
        };

        match self.admit_after_begin(message, key) {
            Ok(event) => event,
            Err(error) => self.finish_with_reply(key, error_reply(tag, error)),
        }
    }

    pub fn complete(
        &mut self,
        request: ServerRequest,
        completion: Result<ServerCompletion>,
    ) -> Option<RMessage> {
        if !self.session.requests.finish(request.key) {
            return None;
        }
        self.session.release_reservations(request.key);
        let tag = request.tag();
        let result = match completion {
            Ok(completion) => self.apply_completion(tag, &request.kind, completion),
            Err(error) => Err(error),
        };
        Some(result.unwrap_or_else(|error| error_reply(tag, error)))
    }

    pub fn handle(&mut self, message: TMessage) -> RMessage
    where
        T: FileTree,
    {
        let tag = message.tag();
        let resets_backend = matches!(message, TMessage::Version { .. });
        let event = self.admit(message);
        if resets_backend {
            if let Err(error) = self.tree.reset() {
                self.session.invalidate();
                return error_reply(tag, error);
            }
        }
        match event {
            ServerEvent::Reply(reply) | ServerEvent::Flush { reply, .. } => reply,
            ServerEvent::Dispatch(request) => {
                let tag = request.tag();
                let result = self.perform_request(&request);
                self.complete(request, result)
                    .unwrap_or_else(|| error_reply(tag, Error::from("stale server completion")))
            }
        }
    }

    fn admit_after_begin(&mut self, message: TMessage, key: RequestKey) -> Result<ServerEvent> {
        let tag = key.tag;
        match message {
            TMessage::Version { .. } => unreachable!("Tversion handled before admission"),
            TMessage::Auth {
                afid, uname, aname, ..
            } => {
                self.session.authorize_uname(&uname)?;
                if afid == NOFID {
                    return Err(Error::from_static(ENOAUTH));
                }
                self.session.reserve_new_fid(key, afid)?;
                Ok(ServerEvent::Dispatch(ServerRequest {
                    key,
                    kind: ServerRequestKind::Auth { afid, uname, aname },
                }))
            }
            TMessage::Attach {
                fid,
                afid,
                uname,
                aname,
                ..
            } => {
                self.session.authorize_uname(&uname)?;
                if afid != NOFID {
                    let auth_state = self.session.fid(afid)?;
                    if !auth_state.qid.is_auth() {
                        return Err(Error::from_static(ENOAUTH));
                    }
                    self.session.reserve_existing_fid(key, afid)?;
                }
                self.session.reserve_new_fid(key, fid)?;
                Ok(ServerEvent::Dispatch(ServerRequest {
                    key,
                    kind: ServerRequestKind::Attach {
                        fid,
                        afid,
                        uname,
                        aname,
                    },
                }))
            }
            TMessage::Flush { oldtag, .. } => {
                let outcome = self.session.requests.flush(oldtag)?;
                if let FlushOutcome::Cancelled(cancelled) = outcome {
                    self.session.release_reservations(cancelled);
                }
                let reply = RMessage::Flush { tag };
                let _finished = self.session.requests.finish(key);
                Ok(ServerEvent::Flush { reply, outcome })
            }
            TMessage::Walk {
                fid,
                newfid,
                wnames,
                ..
            } => {
                validate_walk_names(&wnames)?;
                let source = self.session.fid(fid)?;
                if source.open_mode().is_some() {
                    return Err(Error::from_static(EFIDOPEN));
                }
                if !wnames.is_empty() && !source.qid.is_dir() {
                    return Err(Error::from_static(ENOTDIR));
                }
                if newfid == fid {
                    self.session.reserve_existing_fid(key, fid)?;
                } else {
                    self.session.reserve_shared_fid(key, fid)?;
                    self.session.reserve_new_fid(key, newfid)?;
                }
                Ok(ServerEvent::Dispatch(ServerRequest {
                    key,
                    kind: ServerRequestKind::Walk {
                        fid,
                        newfid,
                        start: source.qid,
                        wnames,
                    },
                }))
            }
            TMessage::Open { fid, mode, .. } => {
                let state = self.session.fid(fid)?;
                validate_open(state, mode)?;
                self.session.reserve_existing_fid(key, fid)?;
                Ok(ServerEvent::Dispatch(ServerRequest {
                    key,
                    kind: ServerRequestKind::Open {
                        fid,
                        qid: state.qid,
                        mode,
                    },
                }))
            }
            TMessage::Create {
                fid,
                name,
                perm,
                mode,
                ..
            } => {
                let state = self.session.fid(fid)?;
                validate_create(state, perm, mode)?;
                self.session.reserve_existing_fid(key, fid)?;
                Ok(ServerEvent::Dispatch(ServerRequest {
                    key,
                    kind: ServerRequestKind::Create {
                        fid,
                        qid: state.qid,
                        name,
                        perm,
                        mode,
                    },
                }))
            }
            TMessage::Read {
                fid, offset, count, ..
            } => {
                let state = self.session.fid(fid)?;
                validate_read(state, offset)?;
                if state.qid.is_dir() {
                    self.session.reserve_existing_fid(key, fid)?;
                }
                Ok(ServerEvent::Dispatch(ServerRequest {
                    key,
                    kind: ServerRequestKind::Read {
                        fid,
                        qid: state.qid,
                        offset,
                        count: clamp_read_count(self.session.msize(), count),
                    },
                }))
            }
            TMessage::Write {
                fid, offset, data, ..
            } => {
                let max = usize::try_from(max_write_payload(self.session.msize()))
                    .map_err(|_| Error::from("msize too large"))?;
                if data.len() > max {
                    return Err(Error::from("write exceeds msize"));
                }
                let state = self.session.fid(fid)?;
                validate_write(state)?;
                Ok(ServerEvent::Dispatch(ServerRequest {
                    key,
                    kind: ServerRequestKind::Write {
                        fid,
                        qid: state.qid,
                        offset,
                        data,
                    },
                }))
            }
            TMessage::Clunk { fid, .. } => {
                let state = self.session.retire_fid(key, fid)?;
                Ok(ServerEvent::Dispatch(ServerRequest {
                    key,
                    kind: ServerRequestKind::Clunk {
                        fid,
                        qid: state.qid,
                    },
                }))
            }
            TMessage::Remove { fid, .. } => {
                let state = self.session.retire_fid(key, fid)?;
                Ok(ServerEvent::Dispatch(ServerRequest {
                    key,
                    kind: ServerRequestKind::Remove {
                        fid,
                        qid: state.qid,
                    },
                }))
            }
            TMessage::Stat { fid, .. } => {
                let state = self.session.fid(fid)?;
                Ok(ServerEvent::Dispatch(ServerRequest {
                    key,
                    kind: ServerRequestKind::Stat {
                        fid,
                        qid: state.qid,
                    },
                }))
            }
            TMessage::Wstat { fid, stat, .. } => {
                let state = self.session.fid(fid)?;
                let variant = self
                    .session
                    .variant()
                    .ok_or_else(|| Error::from_static("version not negotiated"))?;
                validate_wstat(state.qid, &stat, variant)?;
                self.session.reserve_existing_fid(key, fid)?;
                Ok(ServerEvent::Dispatch(ServerRequest {
                    key,
                    kind: ServerRequestKind::Wstat {
                        fid,
                        qid: state.qid,
                        stat,
                    },
                }))
            }
            TMessage::Referrals { fid, .. } => {
                let variant = self
                    .session
                    .variant()
                    .ok_or_else(|| Error::from_static("version not negotiated"))?;
                if !variant.supports_referrals() {
                    return Err(Error::from_static(
                        "namespace referrals require negotiated 9P2000.r9p",
                    ));
                }
                let state = self.session.fid(fid)?;
                self.session.reserve_shared_fid(key, fid)?;
                Ok(ServerEvent::Dispatch(ServerRequest {
                    key,
                    kind: ServerRequestKind::Referrals {
                        fid,
                        qid: state.qid,
                    },
                }))
            }
        }
    }

    fn finish_with_reply(&mut self, key: RequestKey, reply: RMessage) -> ServerEvent {
        let _finished = self.session.requests.finish(key);
        self.session.release_reservations(key);
        ServerEvent::Reply(reply)
    }

    fn apply_completion(
        &mut self,
        tag: Tag,
        request: &ServerRequestKind,
        completion: ServerCompletion,
    ) -> Result<RMessage> {
        let variant = self
            .session
            .variant()
            .ok_or_else(|| Error::from_static("version not negotiated"))?;
        match (request, completion) {
            (ServerRequestKind::Auth { afid, .. }, ServerCompletion::Auth { qid }) => {
                if !qid.is_auth() {
                    return Err(Error::from_static(ENOAUTH));
                }
                validate_qid_variant(variant, qid)?;
                self.session
                    .insert_new_fid(*afid, FidState::opened(qid, mode::ORDWR))?;
                Ok(RMessage::Auth { tag, aqid: qid })
            }
            (ServerRequestKind::Attach { fid, .. }, ServerCompletion::Attach { qid }) => {
                validate_qid_variant(variant, qid)?;
                self.session.insert_new_fid(*fid, FidState::new(qid))?;
                Ok(RMessage::Attach { tag, qid })
            }
            (
                ServerRequestKind::Walk {
                    fid,
                    newfid,
                    start,
                    wnames,
                },
                ServerCompletion::Walk { qids },
            ) => {
                if qids.len() > wnames.len() {
                    return Err(Error::from("walk returned too many qids"));
                }
                for qid in &qids {
                    validate_qid_variant(variant, *qid)?;
                }
                if wnames.is_empty() {
                    if newfid != fid {
                        self.session
                            .insert_new_fid(*newfid, FidState::new(*start))?;
                    }
                    return Ok(RMessage::Walk {
                        tag,
                        qids: Vec::new(),
                    });
                }
                if qids.is_empty() {
                    return Err(Error::from_static(ENOENT));
                }
                if qids.len() == wnames.len() {
                    let qid = qids[qids.len() - 1];
                    if newfid == fid {
                        self.session.bind_fid(*fid, FidState::new(qid))?;
                    } else {
                        self.session.insert_new_fid(*newfid, FidState::new(qid))?;
                    }
                }
                Ok(RMessage::Walk { tag, qids })
            }
            (ServerRequestKind::Open { fid, mode, .. }, ServerCompletion::Open(opened)) => {
                validate_qid_variant(variant, opened.qid)?;
                self.session
                    .bind_fid(*fid, FidState::opened(opened.qid, *mode))?;
                Ok(RMessage::Open {
                    tag,
                    qid: opened.qid,
                    iounit: clamp_iounit(self.session.msize(), opened.iounit),
                })
            }
            (ServerRequestKind::Create { fid, mode, .. }, ServerCompletion::Create(opened)) => {
                validate_qid_variant(variant, opened.qid)?;
                self.session
                    .bind_fid(*fid, FidState::opened(opened.qid, *mode))?;
                Ok(RMessage::Create {
                    tag,
                    qid: opened.qid,
                    iounit: clamp_iounit(self.session.msize(), opened.iounit),
                })
            }
            (
                ServerRequestKind::Read {
                    fid,
                    qid,
                    offset,
                    count,
                },
                ServerCompletion::Read(data),
            ) => {
                let data = match data {
                    ReadData::Bytes(bytes) => take_count(bytes, *count)?,
                    ReadData::Directory(stats) => {
                        for stat in &stats {
                            validate_stat_variant(variant, stat)?;
                        }
                        dirread_chunk(&stats, *offset, *count)?
                    }
                };
                if qid.is_dir() {
                    let state = self.session.fid(*fid)?;
                    let count = u64::try_from(data.len())
                        .map_err(|_| Error::from("directory read count too large"))?;
                    let next_offset = offset
                        .checked_add(count)
                        .ok_or_else(|| Error::from("directory offset overflow"))?;
                    self.session
                        .bind_fid(*fid, state.with_directory_offset(next_offset))?;
                }
                Ok(RMessage::Read { tag, data })
            }
            (ServerRequestKind::Write { data, .. }, ServerCompletion::Write { count }) => {
                if usize::try_from(count).map_or(true, |count| count > data.len()) {
                    return Err(Error::from("write completion exceeds request count"));
                }
                Ok(RMessage::Write { tag, count })
            }
            (ServerRequestKind::Clunk { .. }, ServerCompletion::Clunk) => {
                Ok(RMessage::Clunk { tag })
            }
            (ServerRequestKind::Remove { .. }, ServerCompletion::Remove) => {
                Ok(RMessage::Remove { tag })
            }
            (ServerRequestKind::Stat { .. }, ServerCompletion::Stat { stat }) => {
                validate_stat_variant(variant, &stat)?;
                Ok(RMessage::Stat { tag, stat })
            }
            (ServerRequestKind::Wstat { .. }, ServerCompletion::Wstat) => {
                Ok(RMessage::Wstat { tag })
            }
            (ServerRequestKind::Referrals { .. }, ServerCompletion::Referrals { referrals }) => {
                for referral in &referrals {
                    referral.validate()?;
                }
                Ok(RMessage::Referrals { tag, referrals })
            }
            _ => Err(Error::from("completion kind does not match request")),
        }
    }

    fn perform_request(&mut self, request: &ServerRequest) -> Result<ServerCompletion>
    where
        T: FileTree,
    {
        perform_file_tree_request(&mut self.tree, request)
    }
}

fn validate_qid_variant(variant: crate::codec::Variant, qid: Qid) -> Result<()> {
    if qid.is_symlink() && !variant.supports_symlinks() {
        Err(Error::from_static(ESYMLINKDIALECT))
    } else {
        Ok(())
    }
}

fn validate_stat_variant(variant: crate::codec::Variant, stat: &Stat) -> Result<()> {
    validate_qid_variant(variant, stat.qid)?;
    if stat.mode & DMSYMLINK != 0 && !variant.supports_symlinks() {
        Err(Error::from_static(ESYMLINKDIALECT))
    } else {
        Ok(())
    }
}

pub(super) fn perform_file_tree_request<T: FileTree>(
    tree: &mut T,
    request: &ServerRequest,
) -> Result<ServerCompletion> {
    match &request.kind {
        ServerRequestKind::Auth { afid, uname, aname } => tree
            .auth(*afid, uname, aname)
            .map(|qid| ServerCompletion::Auth { qid }),
        ServerRequestKind::Attach {
            fid,
            afid,
            uname,
            aname,
        } => {
            let qid = if *afid == NOFID {
                tree.attach(*fid, uname, aname)?
            } else {
                tree.attach_with_auth(*fid, *afid, uname, aname)?
            };
            Ok(ServerCompletion::Attach { qid })
        }
        ServerRequestKind::Walk {
            fid,
            newfid,
            wnames,
            start,
        } => tree
            .walk(*fid, *newfid, *start, wnames)
            .map(|qids| ServerCompletion::Walk { qids }),
        ServerRequestKind::Open { fid, qid, mode } => {
            tree.open(*fid, *qid, *mode).map(ServerCompletion::Open)
        }
        ServerRequestKind::Create {
            fid,
            qid,
            name,
            perm,
            mode,
        } => tree
            .create(*fid, *qid, name, *perm, *mode)
            .map(ServerCompletion::Create),
        ServerRequestKind::Read {
            fid,
            qid,
            offset,
            count,
        } => tree
            .read(*fid, *qid, *offset, *count)
            .map(ServerCompletion::Read),
        ServerRequestKind::Write {
            fid,
            qid,
            offset,
            data,
        } => tree
            .write(*fid, *qid, *offset, data)
            .map(|count| ServerCompletion::Write { count }),
        ServerRequestKind::Clunk { fid, qid } => {
            tree.clunk(*fid, *qid).map(|()| ServerCompletion::Clunk)
        }
        ServerRequestKind::Remove { fid, qid } => {
            tree.remove(*fid, *qid).map(|()| ServerCompletion::Remove)
        }
        ServerRequestKind::Stat { qid, .. } => {
            tree.stat(*qid).map(|stat| ServerCompletion::Stat { stat })
        }
        ServerRequestKind::Wstat { fid, qid, stat } => tree
            .wstat(*fid, *qid, stat)
            .map(|()| ServerCompletion::Wstat),
        ServerRequestKind::Referrals { fid, qid } => tree
            .referrals(*fid, *qid)
            .map(|referrals| ServerCompletion::Referrals { referrals }),
    }
}

fn validate_open(state: FidState, open_mode: u8) -> Result<()> {
    if state.open_mode().is_some() {
        return Err(Error::from_static(EFIDOPEN));
    }
    if !mode::is_valid(open_mode) {
        return Err(Error::from_static(EBADMODE));
    }
    if state.qid.is_dir() && !mode::is_directory_mode(open_mode) {
        return Err(Error::from_static(EBADMODE));
    }
    Ok(())
}

fn validate_create(state: FidState, perm: u32, open_mode: u8) -> Result<()> {
    if state.open_mode().is_some() {
        return Err(Error::from_static(EFIDOPEN));
    }
    if !state.qid.is_dir() {
        return Err(Error::from_static(ENOTDIR));
    }
    if !mode::is_valid(open_mode) {
        return Err(Error::from_static(EBADMODE));
    }
    if perm & DMDIR != 0 && !mode::is_directory_mode(open_mode) {
        return Err(Error::from_static(EBADMODE));
    }
    Ok(())
}

fn validate_read(state: FidState, offset: u64) -> Result<()> {
    if state.qid.is_auth() {
        return Ok(());
    }
    let open_mode = state
        .open_mode()
        .ok_or_else(|| Error::from_static(EFIDNOTOPEN))?;
    if !mode::permits_read(open_mode) {
        return Err(Error::from_static(EFIDNOTOPEN));
    }
    if state.qid.is_dir() && offset != 0 && offset != state.directory_offset() {
        return Err(Error::from_static(EBADDIROFFSET));
    }
    Ok(())
}

fn validate_write(state: FidState) -> Result<()> {
    if state.qid.is_auth() {
        return Ok(());
    }
    let open_mode = state
        .open_mode()
        .ok_or_else(|| Error::from_static(EFIDNOTOPEN))?;
    if state.qid.is_dir() || !mode::permits_write(open_mode) {
        return Err(Error::from_static(EFIDNOTOPEN));
    }
    Ok(())
}

fn clamp_iounit(msize: u32, iounit: u32) -> u32 {
    if iounit == 0 {
        0
    } else {
        iounit.min(max_iounit(msize))
    }
}
