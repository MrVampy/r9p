use crate::qid::Qid;

pub type Fid = u32;

pub const NOFID: Fid = u32::MAX;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct FidState {
    pub qid: Qid,
    pub open: bool,
}

impl FidState {
    pub const fn new(qid: Qid) -> Self {
        Self { qid, open: false }
    }

    pub const fn opened(qid: Qid) -> Self {
        Self { qid, open: true }
    }
}
