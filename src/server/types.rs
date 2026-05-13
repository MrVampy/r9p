use crate::{
    fid::Fid,
    flush::{FlushOutcome, RequestKey},
    message::{RMessage, Tag},
    qid::Qid,
    stat::Stat,
};

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
