use crate::{
    codec::{clamp_read_count, max_write_payload, DEFAULT_MSIZE, MAX_MSIZE, MIN_MSIZE},
    error::{
        Error, Result, EBADFID, EBADMSIZE, EBADTAG, EBADVERSION, EBADWNAME, EEXIST, EFIDINUSE,
        EFIDLIMIT, ENOAUTH, EPERM,
    },
    fid::{Fid, FidState, NOFID},
    flush::{FlushOutcome, RequestKey, RequestTable},
    message::{RMessage, TMessage, Tag, MAXWELEM},
    qid::Qid,
    stat::{dirread_chunk, Stat},
};
use std::collections::BTreeMap;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    pub default_msize: u32,
    pub max_msize: u32,
    pub max_fids: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            default_msize: DEFAULT_MSIZE,
            max_msize: MAX_MSIZE,
            max_fids: 4096,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenFile {
    pub qid: Qid,
    pub iounit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadData {
    Bytes(Vec<u8>),
    Directory(Vec<Stat>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerEvent {
    Reply(RMessage),
    Dispatch(ServerRequest),
    Flush {
        reply: RMessage,
        outcome: FlushOutcome,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerRequest {
    pub key: RequestKey,
    pub kind: ServerRequestKind,
}

impl ServerRequest {
    pub const fn tag(&self) -> Tag {
        self.key.tag
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerRequestKind {
    Attach {
        fid: Fid,
        afid: Fid,
        uname: Vec<u8>,
        aname: Vec<u8>,
    },
    Walk {
        fid: Fid,
        newfid: Fid,
        start: Qid,
        wnames: Vec<Vec<u8>>,
    },
    Open {
        fid: Fid,
        qid: Qid,
        mode: u8,
    },
    Create {
        fid: Fid,
        qid: Qid,
        name: Vec<u8>,
        perm: u32,
        mode: u8,
    },
    Read {
        fid: Fid,
        qid: Qid,
        offset: u64,
        count: u32,
    },
    Write {
        fid: Fid,
        qid: Qid,
        offset: u64,
        data: Vec<u8>,
    },
    Clunk {
        fid: Fid,
        qid: Qid,
    },
    Remove {
        fid: Fid,
        qid: Qid,
    },
    Stat {
        fid: Fid,
        qid: Qid,
    },
    Wstat {
        fid: Fid,
        qid: Qid,
        stat: Stat,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerCompletion {
    Attach { qid: Qid },
    Walk { qids: Vec<Qid> },
    Open(OpenFile),
    Create(OpenFile),
    Read(ReadData),
    Write { count: u32 },
    Clunk,
    Remove,
    Stat { stat: Stat },
    Wstat,
}

pub trait FileTree {
    fn attach(&mut self, fid: Fid, uname: &[u8], aname: &[u8]) -> Result<Qid>;
    fn walk(&mut self, fid: Fid, newfid: Fid, start: Qid, names: &[Vec<u8>]) -> Result<Vec<Qid>>;
    fn open(&mut self, fid: Fid, qid: Qid, mode: u8) -> Result<OpenFile>;
    fn read(&mut self, fid: Fid, qid: Qid, offset: u64, count: u32) -> Result<ReadData>;
    fn stat(&mut self, qid: Qid) -> Result<Stat>;

    fn create(
        &mut self,
        _fid: Fid,
        _qid: Qid,
        _name: &[u8],
        _perm: u32,
        _mode: u8,
    ) -> Result<OpenFile> {
        Err(Error::from_static(EPERM))
    }

    fn write(&mut self, _fid: Fid, _qid: Qid, _offset: u64, _data: &[u8]) -> Result<u32> {
        Err(Error::from_static(EPERM))
    }

    fn clunk(&mut self, _fid: Fid, _qid: Qid) -> Result<()> {
        Ok(())
    }

    fn remove(&mut self, _fid: Fid, _qid: Qid) -> Result<()> {
        Err(Error::from_static(EPERM))
    }

    fn wstat(&mut self, _fid: Fid, _qid: Qid, _stat: &Stat) -> Result<()> {
        Err(Error::from_static(EPERM))
    }
}

#[derive(Debug)]
pub struct Session {
    config: ServerConfig,
    msize: u32,
    version: Vec<u8>,
    fids: BTreeMap<Fid, FidState>,
    requests: RequestTable,
}

impl Session {
    pub fn new(config: ServerConfig) -> Self {
        Self {
            msize: config.default_msize,
            version: b"9P2000".to_vec(),
            fids: BTreeMap::new(),
            requests: RequestTable::new(),
            config,
        }
    }

    pub fn msize(&self) -> u32 {
        self.msize
    }

    pub fn version(&self) -> &[u8] {
        &self.version
    }

    pub fn fid_count(&self) -> usize {
        self.fids.len()
    }

    pub fn contains_fid(&self, fid: Fid) -> bool {
        self.fids.contains_key(&fid)
    }

    pub fn reset_for_version(&mut self, requested_msize: u32, version: &[u8]) -> Result<()> {
        self.fids.clear();
        self.requests.reset();
        if requested_msize < MIN_MSIZE {
            return Err(Error::from_static(EBADMSIZE));
        }
        if !version.starts_with(b"9P2000") {
            return Err(Error::from_static(EBADVERSION));
        }
        self.msize = requested_msize.min(self.config.max_msize);
        self.version = b"9P2000".to_vec();
        Ok(())
    }

    pub fn bind_fid(&mut self, fid: Fid, state: FidState) -> Result<()> {
        if !self.fids.contains_key(&fid) && self.fids.len() >= self.config.max_fids {
            return Err(Error::from_static(EFIDLIMIT));
        }
        self.fids.insert(fid, state);
        Ok(())
    }

    pub fn insert_new_fid(&mut self, fid: Fid, state: FidState) -> Result<()> {
        if self.fids.contains_key(&fid) {
            return Err(Error::from_static(EFIDINUSE));
        }
        self.bind_fid(fid, state)
    }

    pub fn remove_fid(&mut self, fid: Fid) -> Result<FidState> {
        self.fids
            .remove(&fid)
            .ok_or_else(|| Error::from_static(EBADFID))
    }

    pub fn fid(&self, fid: Fid) -> Result<FidState> {
        self.fids
            .get(&fid)
            .copied()
            .ok_or_else(|| Error::from_static(EBADFID))
    }

    pub fn request_table(&mut self) -> &mut RequestTable {
        &mut self.requests
    }
}

pub struct Server<T> {
    session: Session,
    tree: T,
}

impl<T> Server<T> {
    pub fn new(tree: T) -> Self {
        Self::with_config(tree, ServerConfig::default())
    }

    pub fn with_config(tree: T, config: ServerConfig) -> Self {
        Self {
            session: Session::new(config),
            tree,
        }
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    pub fn tree_mut(&mut self) -> &mut T {
        &mut self.tree
    }

    pub fn into_tree(self) -> T {
        self.tree
    }

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
            TMessage::Auth { .. } => Err(Error::from_static(ENOAUTH)),
            TMessage::Attach {
                fid,
                afid,
                uname,
                aname,
                ..
            } => {
                if afid != NOFID {
                    return Err(Error::from_static(ENOAUTH));
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
            ServerRequestKind::Attach {
                fid, uname, aname, ..
            } => self
                .tree
                .attach(*fid, uname, aname)
                .map(|qid| ServerCompletion::Attach { qid }),
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

pub fn validate_walk_names(wnames: &[Vec<u8>]) -> Result<()> {
    if wnames.len() > MAXWELEM {
        return Err(Error::from("name too long"));
    }
    for name in wnames {
        if name.is_empty()
            || name.contains(&b'/')
            || name.contains(&0)
            || name.len() > u8::MAX as usize
        {
            return Err(Error::from_static(EBADWNAME));
        }
    }
    Ok(())
}

fn take_count(mut bytes: Vec<u8>, count: u32) -> Result<Vec<u8>> {
    let limit = usize::try_from(count).map_err(|_| Error::from("count too large"))?;
    if bytes.len() > limit {
        bytes.truncate(limit);
    }
    Ok(bytes)
}

pub fn error_reply(tag: Tag, error: Error) -> RMessage {
    let ename = if tag == crate::message::NOTAG && error.message() == EBADTAG.as_bytes() {
        EBADTAG.as_bytes().to_vec()
    } else {
        error.into_message()
    };
    RMessage::Error { tag, ename }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct RootOnly {
        root: Qid,
    }

    impl RootOnly {
        fn new() -> Self {
            Self { root: Qid::dir(1) }
        }
    }

    impl FileTree for RootOnly {
        fn attach(&mut self, _fid: Fid, _uname: &[u8], _aname: &[u8]) -> Result<Qid> {
            Ok(self.root)
        }

        fn walk(
            &mut self,
            _fid: Fid,
            _newfid: Fid,
            _start: Qid,
            names: &[Vec<u8>],
        ) -> Result<Vec<Qid>> {
            if names == [b".".to_vec()] {
                Ok(vec![self.root])
            } else {
                Ok(Vec::new())
            }
        }

        fn open(&mut self, _fid: Fid, qid: Qid, _mode: u8) -> Result<OpenFile> {
            Ok(OpenFile { qid, iounit: 0 })
        }

        fn read(&mut self, _fid: Fid, _qid: Qid, _offset: u64, _count: u32) -> Result<ReadData> {
            Ok(ReadData::Bytes(Vec::new()))
        }

        fn stat(&mut self, qid: Qid) -> Result<Stat> {
            Ok(Stat::new(".", qid, crate::qid::DMDIR | 0o500))
        }
    }

    #[test]
    fn tversion_resets_fids_and_requests() -> Result<()> {
        let mut server = Server::with_config(
            RootOnly::new(),
            ServerConfig {
                max_fids: 2,
                ..ServerConfig::default()
            },
        );
        let attached = server.handle(TMessage::Attach {
            tag: 1,
            fid: 1,
            afid: NOFID,
            uname: b"u".to_vec(),
            aname: Vec::new(),
        });
        assert!(matches!(attached, RMessage::Attach { .. }));
        assert_eq!(server.session().fid_count(), 1);

        let version = server.handle(TMessage::Version {
            tag: crate::message::NOTAG,
            msize: 8192,
            version: b"9P2000".to_vec(),
        });
        assert!(matches!(version, RMessage::Version { .. }));
        assert_eq!(server.session().fid_count(), 0);
        Ok(())
    }

    #[test]
    fn live_fid_cap_counts_fids_not_numeric_values() -> Result<()> {
        let mut server = Server::with_config(
            RootOnly::new(),
            ServerConfig {
                max_fids: 1,
                ..ServerConfig::default()
            },
        );
        let attached = server.handle(TMessage::Attach {
            tag: 1,
            fid: 4_000_000,
            afid: NOFID,
            uname: b"u".to_vec(),
            aname: Vec::new(),
        });
        assert!(matches!(attached, RMessage::Attach { .. }));

        let walk = server.handle(TMessage::Walk {
            tag: 2,
            fid: 4_000_000,
            newfid: 4_000_001,
            wnames: Vec::new(),
        });
        assert_eq!(
            walk,
            RMessage::Error {
                tag: 2,
                ename: EFIDLIMIT.as_bytes().to_vec()
            }
        );
        Ok(())
    }

    #[test]
    fn bad_walk_names_are_rejected_before_backend() -> Result<()> {
        let mut server = Server::new(RootOnly::new());
        let _reply = server.handle(TMessage::Attach {
            tag: 1,
            fid: 1,
            afid: NOFID,
            uname: b"u".to_vec(),
            aname: Vec::new(),
        });
        let walk = server.handle(TMessage::Walk {
            tag: 2,
            fid: 1,
            newfid: 2,
            wnames: vec![b"a/b".to_vec()],
        });
        assert_eq!(
            walk,
            RMessage::Error {
                tag: 2,
                ename: EBADWNAME.as_bytes().to_vec()
            }
        );
        Ok(())
    }

    #[test]
    fn remove_clunks_fid_even_when_backend_rejects_remove() -> Result<()> {
        let mut server = Server::new(RootOnly::new());
        let attach = server.handle(TMessage::Attach {
            tag: 1,
            fid: 1,
            afid: NOFID,
            uname: b"u".to_vec(),
            aname: Vec::new(),
        });
        assert!(matches!(attach, RMessage::Attach { .. }));
        assert!(server.session().contains_fid(1));

        let remove = server.handle(TMessage::Remove { tag: 2, fid: 1 });
        assert_eq!(
            remove,
            RMessage::Error {
                tag: 2,
                ename: EPERM.as_bytes().to_vec()
            }
        );
        assert!(!server.session().contains_fid(1));
        Ok(())
    }

    #[test]
    fn split_attach_completion_binds_fid_when_request_is_live() -> Result<()> {
        let mut server = Server::new(RootOnly::new());
        let request = match server.admit(TMessage::Attach {
            tag: 1,
            fid: 1,
            afid: NOFID,
            uname: b"u".to_vec(),
            aname: Vec::new(),
        }) {
            ServerEvent::Dispatch(request) => request,
            other => panic!("expected dispatch, got {other:?}"),
        };

        let reply = server
            .complete(request, Ok(ServerCompletion::Attach { qid: Qid::dir(1) }))
            .ok_or("live completion was dropped")?;

        assert_eq!(
            reply,
            RMessage::Attach {
                tag: 1,
                qid: Qid::dir(1)
            }
        );
        assert!(server.session().contains_fid(1));
        Ok(())
    }

    #[test]
    fn split_flush_cancels_request_and_drops_stale_completion() -> Result<()> {
        let mut server = Server::new(RootOnly::new());
        let request = match server.admit(TMessage::Attach {
            tag: 1,
            fid: 1,
            afid: NOFID,
            uname: b"u".to_vec(),
            aname: Vec::new(),
        }) {
            ServerEvent::Dispatch(request) => request,
            other => panic!("expected dispatch, got {other:?}"),
        };

        let expected_key = request.key;
        let flush = server.admit(TMessage::Flush { tag: 2, oldtag: 1 });
        assert_eq!(
            flush,
            ServerEvent::Flush {
                reply: RMessage::Flush { tag: 2 },
                outcome: FlushOutcome::Cancelled(expected_key)
            }
        );

        let late = server.complete(request, Ok(ServerCompletion::Attach { qid: Qid::dir(1) }));
        assert_eq!(late, None);
        assert!(!server.session().contains_fid(1));
        Ok(())
    }

    #[test]
    fn split_api_does_not_require_a_filetree_backend() {
        let mut server = Server::new(());
        let event = server.admit(TMessage::Version {
            tag: crate::message::NOTAG,
            msize: 8192,
            version: b"9P2000".to_vec(),
        });
        assert_eq!(
            event,
            ServerEvent::Reply(RMessage::Version {
                tag: crate::message::NOTAG,
                msize: 8192,
                version: b"9P2000".to_vec()
            })
        );
    }
}
