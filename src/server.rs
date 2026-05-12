use crate::{
    codec::{clamp_read_count, max_write_payload, DEFAULT_MSIZE, MAX_MSIZE, MIN_MSIZE},
    error::{
        Error, Result, EBADFID, EBADMSIZE, EBADTAG, EBADVERSION, EBADWNAME, EEXIST, EFIDINUSE,
        EFIDLIMIT, ENOAUTH, EPERM,
    },
    fid::{Fid, FidState, NOFID},
    flush::{RequestKey, RequestTable},
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

impl<T: FileTree> Server<T> {
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

    pub fn handle(&mut self, message: TMessage) -> RMessage {
        if let TMessage::Version {
            tag,
            msize,
            version,
        } = message
        {
            return match self.session.reset_for_version(msize, &version) {
                Ok(()) => RMessage::Version {
                    tag,
                    msize: self.session.msize(),
                    version: self.session.version().to_vec(),
                },
                Err(error) => error_reply(tag, error),
            };
        }

        let tag = message.tag();
        let key = match self.session.requests.begin(tag) {
            Ok(key) => key,
            Err(error) => return error_reply(tag, error),
        };
        let reply = self.handle_admitted(message, key);
        let _finished = self.session.requests.finish(key);
        reply
    }

    fn handle_admitted(&mut self, message: TMessage, _key: RequestKey) -> RMessage {
        let tag = message.tag();
        let result = match message {
            TMessage::Version { .. } => unreachable!("Tversion handled before admission"),
            TMessage::Auth { .. } => Err(Error::from_static(ENOAUTH)),
            TMessage::Attach {
                fid,
                afid,
                uname,
                aname,
                ..
            } => self.handle_attach(tag, fid, afid, &uname, &aname),
            TMessage::Flush { oldtag, .. } => self.handle_flush(tag, oldtag),
            TMessage::Walk {
                fid,
                newfid,
                wnames,
                ..
            } => self.handle_walk(tag, fid, newfid, &wnames),
            TMessage::Open { fid, mode, .. } => self.handle_open(tag, fid, mode),
            TMessage::Create {
                fid,
                name,
                perm,
                mode,
                ..
            } => self.handle_create(tag, fid, &name, perm, mode),
            TMessage::Read {
                fid, offset, count, ..
            } => self.handle_read(tag, fid, offset, count),
            TMessage::Write {
                fid, offset, data, ..
            } => self.handle_write(tag, fid, offset, &data),
            TMessage::Clunk { fid, .. } => self.handle_clunk(tag, fid),
            TMessage::Remove { fid, .. } => self.handle_remove(tag, fid),
            TMessage::Stat { fid, .. } => self.handle_stat(tag, fid),
            TMessage::Wstat { fid, stat, .. } => self.handle_wstat(tag, fid, &stat),
        };
        result.unwrap_or_else(|error| error_reply(tag, error))
    }

    fn handle_attach(
        &mut self,
        tag: Tag,
        fid: Fid,
        afid: Fid,
        uname: &[u8],
        aname: &[u8],
    ) -> Result<RMessage> {
        if afid != NOFID {
            return Err(Error::from_static(ENOAUTH));
        }
        let qid = self.tree.attach(fid, uname, aname)?;
        self.session.insert_new_fid(fid, FidState::new(qid))?;
        Ok(RMessage::Attach { tag, qid })
    }

    fn handle_flush(&mut self, tag: Tag, oldtag: Tag) -> Result<RMessage> {
        let _outcome = self.session.requests.flush(oldtag)?;
        Ok(RMessage::Flush { tag })
    }

    fn handle_walk(
        &mut self,
        tag: Tag,
        fid: Fid,
        newfid: Fid,
        wnames: &[Vec<u8>],
    ) -> Result<RMessage> {
        validate_walk_names(wnames)?;
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
            return Ok(RMessage::Walk {
                tag,
                qids: Vec::new(),
            });
        }

        let qids = self.tree.walk(fid, newfid, source.qid, wnames)?;
        if qids.is_empty() {
            return Err(Error::from_static(EEXIST));
        }
        if qids.len() > wnames.len() {
            return Err(Error::from("walk returned too many qids"));
        }
        if qids.len() == wnames.len() {
            let qid = qids[qids.len() - 1];
            if newfid == fid {
                self.session.bind_fid(fid, FidState::new(qid))?;
            } else {
                self.session.insert_new_fid(newfid, FidState::new(qid))?;
            }
        }
        Ok(RMessage::Walk { tag, qids })
    }

    fn handle_open(&mut self, tag: Tag, fid: Fid, mode: u8) -> Result<RMessage> {
        let state = self.session.fid(fid)?;
        let opened = self.tree.open(fid, state.qid, mode)?;
        self.session.bind_fid(fid, FidState::opened(opened.qid))?;
        Ok(RMessage::Open {
            tag,
            qid: opened.qid,
            iounit: opened.iounit,
        })
    }

    fn handle_create(
        &mut self,
        tag: Tag,
        fid: Fid,
        name: &[u8],
        perm: u32,
        mode: u8,
    ) -> Result<RMessage> {
        let state = self.session.fid(fid)?;
        let opened = self.tree.create(fid, state.qid, name, perm, mode)?;
        self.session.bind_fid(fid, FidState::opened(opened.qid))?;
        Ok(RMessage::Create {
            tag,
            qid: opened.qid,
            iounit: opened.iounit,
        })
    }

    fn handle_read(&mut self, tag: Tag, fid: Fid, offset: u64, count: u32) -> Result<RMessage> {
        let state = self.session.fid(fid)?;
        let count = clamp_read_count(self.session.msize(), count);
        let data = match self.tree.read(fid, state.qid, offset, count)? {
            ReadData::Bytes(bytes) => take_count(bytes, count)?,
            ReadData::Directory(stats) => dirread_chunk(&stats, offset, count)?,
        };
        Ok(RMessage::Read { tag, data })
    }

    fn handle_write(&mut self, tag: Tag, fid: Fid, offset: u64, data: &[u8]) -> Result<RMessage> {
        let max = usize::try_from(max_write_payload(self.session.msize()))
            .map_err(|_| Error::from("msize too large"))?;
        if data.len() > max {
            return Err(Error::from("write exceeds msize"));
        }
        let state = self.session.fid(fid)?;
        let count = self.tree.write(fid, state.qid, offset, data)?;
        Ok(RMessage::Write { tag, count })
    }

    fn handle_clunk(&mut self, tag: Tag, fid: Fid) -> Result<RMessage> {
        let state = self.session.remove_fid(fid)?;
        self.tree.clunk(fid, state.qid)?;
        Ok(RMessage::Clunk { tag })
    }

    fn handle_remove(&mut self, tag: Tag, fid: Fid) -> Result<RMessage> {
        let state = self.session.fid(fid)?;
        let result = self.tree.remove(fid, state.qid);
        let _removed = self.session.remove_fid(fid)?;
        result?;
        Ok(RMessage::Remove { tag })
    }

    fn handle_stat(&mut self, tag: Tag, fid: Fid) -> Result<RMessage> {
        let state = self.session.fid(fid)?;
        let stat = self.tree.stat(state.qid)?;
        Ok(RMessage::Stat { tag, stat })
    }

    fn handle_wstat(&mut self, tag: Tag, fid: Fid, stat: &Stat) -> Result<RMessage> {
        let state = self.session.fid(fid)?;
        self.tree.wstat(fid, state.qid, stat)?;
        Ok(RMessage::Wstat { tag })
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
}
