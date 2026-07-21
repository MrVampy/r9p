use crate::{
    error::{Error, Result, EBADTAG, EDUPTAG},
    fid::{Fid, NOFID},
    flush::RequestTable,
    message::{RMessage, TMessage, Tag, NOTAG},
    qid::Qid,
    stat::Stat,
};
use std::collections::BTreeMap;

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
    requested_msize: Option<u32>,
    next_tag: Tag,
    next_fid: Fid,
    pending: RequestTable,
    flush_targets: BTreeMap<Tag, Tag>,
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
            requested_msize: None,
            next_tag: 0,
            next_fid: 1,
            pending: RequestTable::new(),
            flush_targets: BTreeMap::new(),
        }
    }

    pub fn msize(&self) -> u32 {
        self.msize
    }

    pub fn version(&self) -> &[u8] {
        &self.version
    }

    pub fn version_request(&mut self, msize: u32) -> TMessage {
        self.version_request_for(msize, crate::codec::Variant::Plain)
    }

    pub fn version_request_for(&mut self, msize: u32, variant: crate::codec::Variant) -> TMessage {
        self.pending.reset();
        self.flush_targets.clear();
        self.version = b"unknown".to_vec();
        self.requested_msize = Some(msize);
        TMessage::Version {
            tag: NOTAG,
            msize,
            version: variant.wire_name().to_vec(),
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

    pub fn create(
        &mut self,
        fid: Fid,
        name: impl Into<Vec<u8>>,
        perm: u32,
        mode: u8,
    ) -> Result<Op> {
        let tag = self.alloc_tag()?;
        Ok(Op {
            tag,
            fid: Some(fid),
            message: TMessage::Create {
                tag,
                fid,
                name: name.into(),
                perm,
                mode,
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

    pub fn remove(&mut self, fid: Fid) -> Result<Op> {
        let tag = self.alloc_tag()?;
        Ok(Op {
            tag,
            fid: Some(fid),
            message: TMessage::Remove { tag, fid },
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

    pub fn wstat(&mut self, fid: Fid, stat: Stat) -> Result<Op> {
        let tag = self.alloc_tag()?;
        Ok(Op {
            tag,
            fid: Some(fid),
            message: TMessage::Wstat { tag, fid, stat },
        })
    }

    pub fn flush(&mut self, oldtag: Tag) -> Result<Op> {
        if oldtag == NOTAG {
            return Err(Error::from_static(EBADTAG));
        }
        let tag = self.alloc_tag()?;
        self.flush_targets.insert(tag, oldtag);
        Ok(Op {
            tag,
            fid: None,
            message: TMessage::Flush { tag, oldtag },
        })
    }

    pub(crate) fn abandon(&mut self, tag: Tag) {
        if tag == NOTAG {
            self.requested_msize = None;
        } else {
            let _ = self.pending.flush(tag);
            self.flush_targets.remove(&tag);
        }
    }

    pub fn receive(&mut self, response: RMessage) -> Result<ClientResponse> {
        match response {
            RMessage::Version {
                tag,
                msize,
                version,
            } => {
                if tag != NOTAG {
                    return Err(Error::from("Rversion did not use NOTAG"));
                }
                let requested_msize = self
                    .requested_msize
                    .take()
                    .ok_or_else(|| Error::from("unsolicited Rversion"))?;
                if msize < crate::codec::MIN_MSIZE || msize > requested_msize {
                    return Err(Error::from(format!(
                        "invalid negotiated msize {msize} for request {requested_msize}"
                    )));
                }
                self.msize = msize;
                self.version = version.clone();
                self.pending.reset();
                self.flush_targets.clear();
                Ok(ClientResponse::Completion {
                    tag,
                    completion: Completion::Version { msize, version },
                })
            }
            RMessage::Error { tag, ename } => {
                if tag == NOTAG {
                    self.requested_msize
                        .take()
                        .ok_or_else(|| Error::from("unknown response tag"))?;
                } else {
                    self.finish(tag)?;
                }
                self.flush_targets.remove(&tag);
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
                if let Some(oldtag) = self.flush_targets.remove(&tag) {
                    let _ = self.pending.flush(oldtag);
                }
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
                validate_iounit(self.msize, iounit)?;
                Ok(ClientResponse::Completion {
                    tag,
                    completion: Completion::Open { qid, iounit },
                })
            }
            RMessage::Create { tag, qid, iounit } => {
                self.finish(tag)?;
                validate_iounit(self.msize, iounit)?;
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

fn validate_iounit(msize: u32, iounit: u32) -> Result<()> {
    if iounit != 0 && iounit > crate::codec::max_iounit(msize) {
        Err(Error::from(format!(
            "iounit {iounit} exceeds negotiated msize {msize}"
        )))
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Op {
    pub tag: Tag,
    pub fid: Option<Fid>,
    pub message: TMessage,
}

#[cfg(test)]
mod tests {
    use super::{Client, Op};
    use crate::{message::TMessage, qid::Qid, stat::Stat, RMessage, NOTAG};

    #[test]
    fn version_reply_must_match_an_outstanding_notag_negotiation() {
        let mut client = Client::new();
        assert!(client
            .receive(RMessage::Version {
                tag: NOTAG,
                msize: 8192,
                version: b"9P2000".to_vec(),
            })
            .is_err());

        let _request = client.version_request(8192);
        assert!(client
            .receive(RMessage::Version {
                tag: 1,
                msize: 8192,
                version: b"9P2000".to_vec(),
            })
            .is_err());
    }

    #[test]
    fn version_reply_cannot_expand_or_shrink_below_the_protocol_minimum() {
        let mut client = Client::new();
        let _request = client.version_request(8192);
        assert!(client
            .receive(RMessage::Version {
                tag: NOTAG,
                msize: 8193,
                version: b"9P2000".to_vec(),
            })
            .is_err());

        let _request = client.version_request(8192);
        assert!(client
            .receive(RMessage::Version {
                tag: NOTAG,
                msize: crate::codec::MIN_MSIZE - 1,
                version: b"9P2000".to_vec(),
            })
            .is_err());
    }

    #[test]
    fn error_reply_requires_a_live_request_tag() {
        let mut client = Client::new();

        assert!(client
            .receive(RMessage::Error {
                tag: 7,
                ename: b"late".to_vec(),
            })
            .is_err());
    }

    #[test]
    fn locally_abandoned_request_does_not_leave_a_live_tag() {
        let mut client = Client::new();
        let request = client.stat(1).expect("stat request");
        client.abandon(request.tag);

        assert!(client
            .receive(RMessage::Stat {
                tag: request.tag,
                stat: Stat::new("late", Qid::file(1), 0o444),
            })
            .is_err());
    }

    #[test]
    fn open_reply_iounit_cannot_exceed_the_negotiated_payload() {
        let mut client = Client::new();
        let request = client.open(1, crate::OREAD).expect("open request");
        let tag = request.tag;

        assert!(client
            .receive(RMessage::Open {
                tag,
                qid: Qid::file(1),
                iounit: crate::codec::max_iounit(client.msize()) + 1,
            })
            .is_err());
        assert!(!client.pending.is_live(tag));
    }

    #[test]
    fn builds_create_remove_and_wstat_ops() {
        let mut client = Client::new();

        let create = must_op(client.create(3, b"created".to_vec(), 0o644, 1));
        assert_eq!(create.fid, Some(3));
        match create.message {
            TMessage::Create {
                tag,
                fid,
                name,
                perm,
                mode,
            } => {
                assert_eq!(tag, create.tag);
                assert_eq!(fid, 3);
                assert_eq!(name, b"created".to_vec());
                assert_eq!(perm, 0o644);
                assert_eq!(mode, 1);
            }
            other => panic!("expected Tcreate, got {other:?}"),
        }

        let remove = must_op(client.remove(4));
        assert_eq!(remove.fid, Some(4));
        match remove.message {
            TMessage::Remove { tag, fid } => {
                assert_eq!(tag, remove.tag);
                assert_eq!(fid, 4);
            }
            other => panic!("expected Tremove, got {other:?}"),
        }

        let mut stat = Stat::null_wstat();
        stat.name = b"renamed".to_vec();
        let wstat = must_op(client.wstat(5, stat.clone()));
        assert_eq!(wstat.fid, Some(5));
        match wstat.message {
            TMessage::Wstat {
                tag,
                fid,
                stat: actual,
            } => {
                assert_eq!(tag, wstat.tag);
                assert_eq!(fid, 5);
                assert_eq!(actual, stat);
            }
            other => panic!("expected Twstat, got {other:?}"),
        }
    }

    fn must_op(result: crate::Result<Op>) -> Op {
        match result {
            Ok(op) => op,
            Err(error) => panic!("client op failed: {error}"),
        }
    }
}
