use crate::{
    codec::{clamp_read_count, max_write_payload},
    error::{Error, Result, EEXIST, EFIDINUSE, EFIDLIMIT, ENOAUTH},
    fid::{FidState, NOFID},
    flush::RequestKey,
    message::{RMessage, TMessage, Tag},
    stat::dirread_chunk,
};

use super::{
    types::{ReadData, ServerCompletion, ServerEvent, ServerRequest, ServerRequestKind},
    validation::{error_reply, take_count, validate_walk_names},
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
                Ok(()) => ServerEvent::Reply(RMessage::Version {
                    tag,
                    msize: self.session.msize(),
                    version: self.session.version().to_vec(),
                }),
                Err(error) => ServerEvent::Reply(error_reply(tag, error)),
            };
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
        match self.admit(message) {
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
                if afid == NOFID {
                    return Err(Error::from_static(ENOAUTH));
                }
                if self.session.contains_fid(afid) {
                    return Err(Error::from_static(EFIDINUSE));
                }
                if self.session.fid_count() >= self.session.config.max_fids {
                    return Err(Error::from_static(EFIDLIMIT));
                }
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
                if afid != NOFID {
                    let auth_state = self.session.fid(afid)?;
                    if !auth_state.qid.is_auth() {
                        return Err(Error::from_static(ENOAUTH));
                    }
                }
                if self.session.contains_fid(fid) {
                    return Err(Error::from_static(EFIDINUSE));
                }
                if self.session.fid_count() >= self.session.config.max_fids {
                    return Err(Error::from_static(EFIDLIMIT));
                }
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
                if newfid != fid && self.session.contains_fid(newfid) {
                    return Err(Error::from_static(EFIDINUSE));
                }
                if newfid != fid
                    && !self.session.contains_fid(newfid)
                    && self.session.fid_count() >= self.session.config.max_fids
                {
                    return Err(Error::from_static(EFIDLIMIT));
                }
                if wnames.is_empty() {
                    if newfid != fid {
                        self.session.insert_new_fid(newfid, source)?;
                    }
                    return Ok(self.finish_with_reply(
                        key,
                        RMessage::Walk {
                            tag,
                            qids: Vec::new(),
                        },
                    ));
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
                let state = self.session.remove_fid(fid)?;
                Ok(ServerEvent::Dispatch(ServerRequest {
                    key,
                    kind: ServerRequestKind::Clunk {
                        fid,
                        qid: state.qid,
                    },
                }))
            }
            TMessage::Remove { fid, .. } => {
                let state = self.session.remove_fid(fid)?;
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
                Ok(ServerEvent::Dispatch(ServerRequest {
                    key,
                    kind: ServerRequestKind::Wstat {
                        fid,
                        qid: state.qid,
                        stat,
                    },
                }))
            }
        }
    }

    fn finish_with_reply(&mut self, key: RequestKey, reply: RMessage) -> ServerEvent {
        let _finished = self.session.requests.finish(key);
        ServerEvent::Reply(reply)
    }

    fn apply_completion(
        &mut self,
        tag: Tag,
        request: &ServerRequestKind,
        completion: ServerCompletion,
    ) -> Result<RMessage> {
        match (request, completion) {
            (ServerRequestKind::Auth { afid, .. }, ServerCompletion::Auth { qid }) => {
                if !qid.is_auth() {
                    return Err(Error::from_static(ENOAUTH));
                }
                self.session.insert_new_fid(*afid, FidState::new(qid))?;
                Ok(RMessage::Auth { tag, aqid: qid })
            }
            (ServerRequestKind::Attach { fid, .. }, ServerCompletion::Attach { qid }) => {
                self.session.insert_new_fid(*fid, FidState::new(qid))?;
                Ok(RMessage::Attach { tag, qid })
            }
            (
                ServerRequestKind::Walk {
                    fid,
                    newfid,
                    wnames,
                    ..
                },
                ServerCompletion::Walk { qids },
            ) => {
                if qids.is_empty() {
                    return Err(Error::from_static(EEXIST));
                }
                if qids.len() > wnames.len() {
                    return Err(Error::from("walk returned too many qids"));
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
            (ServerRequestKind::Open { fid, .. }, ServerCompletion::Open(opened)) => {
                self.session.bind_fid(*fid, FidState::opened(opened.qid))?;
                Ok(RMessage::Open {
                    tag,
                    qid: opened.qid,
                    iounit: opened.iounit,
                })
            }
            (ServerRequestKind::Create { fid, .. }, ServerCompletion::Create(opened)) => {
                self.session.bind_fid(*fid, FidState::opened(opened.qid))?;
                Ok(RMessage::Create {
                    tag,
                    qid: opened.qid,
                    iounit: opened.iounit,
                })
            }
            (ServerRequestKind::Read { offset, count, .. }, ServerCompletion::Read(data)) => {
                let data = match data {
                    ReadData::Bytes(bytes) => take_count(bytes, *count)?,
                    ReadData::Directory(stats) => dirread_chunk(&stats, *offset, *count)?,
                };
                Ok(RMessage::Read { tag, data })
            }
            (ServerRequestKind::Write { .. }, ServerCompletion::Write { count }) => {
                Ok(RMessage::Write { tag, count })
            }
            (ServerRequestKind::Clunk { .. }, ServerCompletion::Clunk) => {
                Ok(RMessage::Clunk { tag })
            }
            (ServerRequestKind::Remove { .. }, ServerCompletion::Remove) => {
                Ok(RMessage::Remove { tag })
            }
            (ServerRequestKind::Stat { .. }, ServerCompletion::Stat { stat }) => {
                Ok(RMessage::Stat { tag, stat })
            }
            (ServerRequestKind::Wstat { .. }, ServerCompletion::Wstat) => {
                Ok(RMessage::Wstat { tag })
            }
            _ => Err(Error::from("completion kind does not match request")),
        }
    }

    fn perform_request(&mut self, request: &ServerRequest) -> Result<ServerCompletion>
    where
        T: FileTree,
    {
        match &request.kind {
            ServerRequestKind::Auth { afid, uname, aname } => self
                .tree
                .auth(*afid, uname, aname)
                .map(|qid| ServerCompletion::Auth { qid }),
            ServerRequestKind::Attach {
                fid,
                afid,
                uname,
                aname,
            } => {
                let qid = if *afid == NOFID {
                    self.tree.attach(*fid, uname, aname)?
                } else {
                    self.tree.attach_with_auth(*fid, *afid, uname, aname)?
                };
                Ok(ServerCompletion::Attach { qid })
            }
            ServerRequestKind::Walk {
                fid,
                newfid,
                wnames,
                start,
            } => self
                .tree
                .walk(*fid, *newfid, *start, wnames)
                .map(|qids| ServerCompletion::Walk { qids }),
            ServerRequestKind::Open { fid, qid, mode } => self
                .tree
                .open(*fid, *qid, *mode)
                .map(ServerCompletion::Open),
            ServerRequestKind::Create {
                fid,
                qid,
                name,
                perm,
                mode,
            } => self
                .tree
                .create(*fid, *qid, name, *perm, *mode)
                .map(ServerCompletion::Create),
            ServerRequestKind::Read {
                fid,
                qid,
                offset,
                count,
            } => self
                .tree
                .read(*fid, *qid, *offset, *count)
                .map(ServerCompletion::Read),
            ServerRequestKind::Write {
                fid,
                qid,
                offset,
                data,
            } => self
                .tree
                .write(*fid, *qid, *offset, data)
                .map(|count| ServerCompletion::Write { count }),
            ServerRequestKind::Clunk { fid, qid } => self
                .tree
                .clunk(*fid, *qid)
                .map(|()| ServerCompletion::Clunk),
            ServerRequestKind::Remove { fid, qid } => self
                .tree
                .remove(*fid, *qid)
                .map(|()| ServerCompletion::Remove),
            ServerRequestKind::Stat { qid, .. } => self
                .tree
                .stat(*qid)
                .map(|stat| ServerCompletion::Stat { stat }),
            ServerRequestKind::Wstat { fid, qid, stat } => self
                .tree
                .wstat(*fid, *qid, stat)
                .map(|()| ServerCompletion::Wstat),
        }
    }
}
