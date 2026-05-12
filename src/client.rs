use crate::{
    error::{Error, Result, EDUPTAG},
    fid::{Fid, NOFID},
    flush::RequestTable,
    message::{RMessage, TMessage, Tag, NOTAG},
    qid::Qid,
    stat::Stat,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Completion {
    Version { msize: u32, version: Vec<u8> },
    Attach { qid: Qid },
    Auth { aqid: Qid },
    Flush,
    Walk { qids: Vec<Qid> },
    Open { qid: Qid, iounit: u32 },
    Create { qid: Qid, iounit: u32 },
    Read { data: Vec<u8> },
    Write { count: u32 },
    Clunk,
    Remove,
    Stat { stat: Stat },
    Wstat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientResponse {
    Completion { tag: Tag, completion: Completion },
    Error { tag: Tag, ename: Vec<u8> },
}

#[derive(Debug)]
pub struct Client {
    msize: u32,
    version: Vec<u8>,
    next_tag: Tag,
    next_fid: Fid,
    pending: RequestTable,
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    pub fn new() -> Self {
        Self {
            msize: crate::codec::DEFAULT_MSIZE,
            version: b"9P2000".to_vec(),
            next_tag: 0,
            next_fid: 1,
            pending: RequestTable::new(),
        }
    }

    pub fn msize(&self) -> u32 {
        self.msize
    }

    pub fn version(&self) -> &[u8] {
        &self.version
    }

    pub fn version_request(&mut self, msize: u32) -> TMessage {
        self.pending.reset();
        TMessage::Version {
            tag: NOTAG,
            msize,
            version: b"9P2000".to_vec(),
        }
    }

    pub fn attach(&mut self, uname: impl Into<Vec<u8>>, aname: impl Into<Vec<u8>>) -> Result<Op> {
        let tag = self.alloc_tag()?;
        let fid = self.alloc_fid();
        Ok(Op {
            tag,
            fid: Some(fid),
            message: TMessage::Attach {
                tag,
                fid,
                afid: NOFID,
                uname: uname.into(),
                aname: aname.into(),
            },
        })
    }

    pub fn walk(&mut self, fid: Fid, wnames: Vec<Vec<u8>>) -> Result<Op> {
        let tag = self.alloc_tag()?;
        let newfid = self.alloc_fid();
        Ok(Op {
            tag,
            fid: Some(newfid),
            message: TMessage::Walk {
                tag,
                fid,
                newfid,
                wnames,
            },
        })
    }

    pub fn open(&mut self, fid: Fid, mode: u8) -> Result<Op> {
        let tag = self.alloc_tag()?;
        Ok(Op {
            tag,
            fid: Some(fid),
            message: TMessage::Open { tag, fid, mode },
        })
    }

    pub fn read(&mut self, fid: Fid, offset: u64, count: u32) -> Result<Op> {
        let tag = self.alloc_tag()?;
        Ok(Op {
            tag,
            fid: Some(fid),
            message: TMessage::Read {
                tag,
                fid,
                offset,
                count,
            },
        })
    }

    pub fn write(&mut self, fid: Fid, offset: u64, data: impl Into<Vec<u8>>) -> Result<Op> {
        let tag = self.alloc_tag()?;
        Ok(Op {
            tag,
            fid: Some(fid),
            message: TMessage::Write {
                tag,
                fid,
                offset,
                data: data.into(),
            },
        })
    }

    pub fn clunk(&mut self, fid: Fid) -> Result<Op> {
        let tag = self.alloc_tag()?;
        Ok(Op {
            tag,
            fid: Some(fid),
            message: TMessage::Clunk { tag, fid },
        })
    }

    pub fn stat(&mut self, fid: Fid) -> Result<Op> {
        let tag = self.alloc_tag()?;
        Ok(Op {
            tag,
            fid: Some(fid),
            message: TMessage::Stat { tag, fid },
        })
    }

    pub fn flush(&mut self, oldtag: Tag) -> Result<Op> {
        let tag = self.alloc_tag()?;
        Ok(Op {
            tag,
            fid: None,
            message: TMessage::Flush { tag, oldtag },
        })
    }

    pub fn receive(&mut self, response: RMessage) -> Result<ClientResponse> {
        match response {
            RMessage::Version {
                tag,
                msize,
                version,
            } => {
                self.msize = msize;
                self.version = version.clone();
                self.pending.reset();
                Ok(ClientResponse::Completion {
                    tag,
                    completion: Completion::Version { msize, version },
                })
            }
            RMessage::Error { tag, ename } => {
                let _ = self.pending.flush(tag);
                Ok(ClientResponse::Error { tag, ename })
            }
            RMessage::Attach { tag, qid } => {
                self.finish(tag)?;
                Ok(ClientResponse::Completion {
                    tag,
                    completion: Completion::Attach { qid },
                })
            }
            RMessage::Auth { tag, aqid } => {
                self.finish(tag)?;
                Ok(ClientResponse::Completion {
                    tag,
                    completion: Completion::Auth { aqid },
                })
            }
            RMessage::Flush { tag } => {
                self.finish(tag)?;
                Ok(ClientResponse::Completion {
                    tag,
                    completion: Completion::Flush,
                })
            }
            RMessage::Walk { tag, qids } => {
                self.finish(tag)?;
                Ok(ClientResponse::Completion {
                    tag,
                    completion: Completion::Walk { qids },
                })
            }
            RMessage::Open { tag, qid, iounit } => {
                self.finish(tag)?;
                Ok(ClientResponse::Completion {
                    tag,
                    completion: Completion::Open { qid, iounit },
                })
            }
            RMessage::Create { tag, qid, iounit } => {
                self.finish(tag)?;
                Ok(ClientResponse::Completion {
                    tag,
                    completion: Completion::Create { qid, iounit },
                })
            }
            RMessage::Read { tag, data } => {
                self.finish(tag)?;
                Ok(ClientResponse::Completion {
                    tag,
                    completion: Completion::Read { data },
                })
            }
            RMessage::Write { tag, count } => {
                self.finish(tag)?;
                Ok(ClientResponse::Completion {
                    tag,
                    completion: Completion::Write { count },
                })
            }
            RMessage::Clunk { tag } => {
                self.finish(tag)?;
                Ok(ClientResponse::Completion {
                    tag,
                    completion: Completion::Clunk,
                })
            }
            RMessage::Remove { tag } => {
                self.finish(tag)?;
                Ok(ClientResponse::Completion {
                    tag,
                    completion: Completion::Remove,
                })
            }
            RMessage::Stat { tag, stat } => {
                self.finish(tag)?;
                Ok(ClientResponse::Completion {
                    tag,
                    completion: Completion::Stat { stat },
                })
            }
            RMessage::Wstat { tag } => {
                self.finish(tag)?;
                Ok(ClientResponse::Completion {
                    tag,
                    completion: Completion::Wstat,
                })
            }
        }
    }

    fn alloc_tag(&mut self) -> Result<Tag> {
        for _ in 0..u16::MAX {
            let tag = self.next_tag;
            self.next_tag = self.next_tag.wrapping_add(1);
            if tag == NOTAG {
                continue;
            }
            if !self.pending.is_live(tag) {
                let _key = self.pending.begin(tag)?;
                return Ok(tag);
            }
        }
        Err(Error::from_static(EDUPTAG))
    }

    fn alloc_fid(&mut self) -> Fid {
        let fid = self.next_fid;
        self.next_fid = self.next_fid.wrapping_add(1);
        if self.next_fid == NOFID {
            self.next_fid = 1;
        }
        fid
    }

    fn finish(&mut self, tag: Tag) -> Result<()> {
        let outcome = self.pending.flush(tag)?;
        match outcome {
            crate::flush::FlushOutcome::Cancelled(_) => Ok(()),
            crate::flush::FlushOutcome::Unknown => Err(Error::from("unknown response tag")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Op {
    pub tag: Tag,
    pub fid: Option<Fid>,
    pub message: TMessage,
}
