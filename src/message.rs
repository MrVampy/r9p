use crate::{fid::Fid, qid::Qid, stat::Stat};

pub type Tag = u16;

pub const NOTAG: Tag = u16::MAX;
pub const MAXWELEM: usize = 16;

pub const TVERSION: u8 = 100;
pub const RVERSION: u8 = 101;
pub const TAUTH: u8 = 102;
pub const RAUTH: u8 = 103;
pub const TATTACH: u8 = 104;
pub const RATTACH: u8 = 105;
pub const RERROR: u8 = 107;
pub const TFLUSH: u8 = 108;
pub const RFLUSH: u8 = 109;
pub const TWALK: u8 = 110;
pub const RWALK: u8 = 111;
pub const TOPEN: u8 = 112;
pub const ROPEN: u8 = 113;
pub const TCREATE: u8 = 114;
pub const RCREATE: u8 = 115;
pub const TREAD: u8 = 116;
pub const RREAD: u8 = 117;
pub const TWRITE: u8 = 118;
pub const RWRITE: u8 = 119;
pub const TCLUNK: u8 = 120;
pub const RCLUNK: u8 = 121;
pub const TREMOVE: u8 = 122;
pub const RREMOVE: u8 = 123;
pub const TSTAT: u8 = 124;
pub const RSTAT: u8 = 125;
pub const TWSTAT: u8 = 126;
pub const RWSTAT: u8 = 127;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TMessage {
    Version {
        tag: Tag,
        msize: u32,
        version: Vec<u8>,
    },
    Auth {
        tag: Tag,
        afid: Fid,
        uname: Vec<u8>,
        aname: Vec<u8>,
    },
    Attach {
        tag: Tag,
        fid: Fid,
        afid: Fid,
        uname: Vec<u8>,
        aname: Vec<u8>,
    },
    Flush {
        tag: Tag,
        oldtag: Tag,
    },
    Walk {
        tag: Tag,
        fid: Fid,
        newfid: Fid,
        wnames: Vec<Vec<u8>>,
    },
    Open {
        tag: Tag,
        fid: Fid,
        mode: u8,
    },
    Create {
        tag: Tag,
        fid: Fid,
        name: Vec<u8>,
        perm: u32,
        mode: u8,
    },
    Read {
        tag: Tag,
        fid: Fid,
        offset: u64,
        count: u32,
    },
    Write {
        tag: Tag,
        fid: Fid,
        offset: u64,
        data: Vec<u8>,
    },
    Clunk {
        tag: Tag,
        fid: Fid,
    },
    Remove {
        tag: Tag,
        fid: Fid,
    },
    Stat {
        tag: Tag,
        fid: Fid,
    },
    Wstat {
        tag: Tag,
        fid: Fid,
        stat: Stat,
    },
}

impl TMessage {
    pub const fn tag(&self) -> Tag {
        match self {
            Self::Version { tag, .. }
            | Self::Auth { tag, .. }
            | Self::Attach { tag, .. }
            | Self::Flush { tag, .. }
            | Self::Walk { tag, .. }
            | Self::Open { tag, .. }
            | Self::Create { tag, .. }
            | Self::Read { tag, .. }
            | Self::Write { tag, .. }
            | Self::Clunk { tag, .. }
            | Self::Remove { tag, .. }
            | Self::Stat { tag, .. }
            | Self::Wstat { tag, .. } => *tag,
        }
    }

    pub const fn message_type(&self) -> u8 {
        match self {
            Self::Version { .. } => TVERSION,
            Self::Auth { .. } => TAUTH,
            Self::Attach { .. } => TATTACH,
            Self::Flush { .. } => TFLUSH,
            Self::Walk { .. } => TWALK,
            Self::Open { .. } => TOPEN,
            Self::Create { .. } => TCREATE,
            Self::Read { .. } => TREAD,
            Self::Write { .. } => TWRITE,
            Self::Clunk { .. } => TCLUNK,
            Self::Remove { .. } => TREMOVE,
            Self::Stat { .. } => TSTAT,
            Self::Wstat { .. } => TWSTAT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RMessage {
    Version {
        tag: Tag,
        msize: u32,
        version: Vec<u8>,
    },
    Auth {
        tag: Tag,
        aqid: Qid,
    },
    Attach {
        tag: Tag,
        qid: Qid,
    },
    Error {
        tag: Tag,
        ename: Vec<u8>,
    },
    Flush {
        tag: Tag,
    },
    Walk {
        tag: Tag,
        qids: Vec<Qid>,
    },
    Open {
        tag: Tag,
        qid: Qid,
        iounit: u32,
    },
    Create {
        tag: Tag,
        qid: Qid,
        iounit: u32,
    },
    Read {
        tag: Tag,
        data: Vec<u8>,
    },
    Write {
        tag: Tag,
        count: u32,
    },
    Clunk {
        tag: Tag,
    },
    Remove {
        tag: Tag,
    },
    Stat {
        tag: Tag,
        stat: Stat,
    },
    Wstat {
        tag: Tag,
    },
}

impl RMessage {
    pub const fn tag(&self) -> Tag {
        match self {
            Self::Version { tag, .. }
            | Self::Auth { tag, .. }
            | Self::Attach { tag, .. }
            | Self::Error { tag, .. }
            | Self::Flush { tag }
            | Self::Walk { tag, .. }
            | Self::Open { tag, .. }
            | Self::Create { tag, .. }
            | Self::Read { tag, .. }
            | Self::Write { tag, .. }
            | Self::Clunk { tag }
            | Self::Remove { tag }
            | Self::Stat { tag, .. }
            | Self::Wstat { tag } => *tag,
        }
    }

    pub const fn message_type(&self) -> u8 {
        match self {
            Self::Version { .. } => RVERSION,
            Self::Auth { .. } => RAUTH,
            Self::Attach { .. } => RATTACH,
            Self::Error { .. } => RERROR,
            Self::Flush { .. } => RFLUSH,
            Self::Walk { .. } => RWALK,
            Self::Open { .. } => ROPEN,
            Self::Create { .. } => RCREATE,
            Self::Read { .. } => RREAD,
            Self::Write { .. } => RWRITE,
            Self::Clunk { .. } => RCLUNK,
            Self::Remove { .. } => RREMOVE,
            Self::Stat { .. } => RSTAT,
            Self::Wstat { .. } => RWSTAT,
        }
    }
}
