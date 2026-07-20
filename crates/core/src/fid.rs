use crate::qid::Qid;

pub type Fid = u32;

pub const NOFID: Fid = u32::MAX;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct FidState {
    pub qid: Qid,
    open_mode: Option<u8>,
    directory_offset: u64,
}

impl FidState {
    pub const fn new(qid: Qid) -> Self {
        Self {
            qid,
            open_mode: None,
            directory_offset: 0,
        }
    }

    pub const fn opened(qid: Qid, mode: u8) -> Self {
        Self {
            qid,
            open_mode: Some(mode),
            directory_offset: 0,
        }
    }

    pub const fn open_mode(self) -> Option<u8> {
        self.open_mode
    }

    pub const fn directory_offset(self) -> u64 {
        self.directory_offset
    }

    pub const fn with_directory_offset(mut self, offset: u64) -> Self {
        self.directory_offset = offset;
        self
    }
}
