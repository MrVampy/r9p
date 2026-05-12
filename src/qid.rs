pub const QTFILE: u8 = 0x00;
pub const QTDIR: u8 = 0x80;
pub const QTAPPEND: u8 = 0x40;

pub const DMDIR: u32 = 0x8000_0000;
pub const DMAPPEND: u32 = 0x4000_0000;

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Qid {
    pub qtype: u8,
    pub version: u32,
    pub path: u64,
}

impl Qid {
    pub const fn new(qtype: u8, version: u32, path: u64) -> Self {
        Self {
            qtype,
            version,
            path,
        }
    }

    pub const fn file(path: u64) -> Self {
        Self::new(QTFILE, 0, path)
    }

    pub const fn dir(path: u64) -> Self {
        Self::new(QTDIR, 0, path)
    }

    pub const fn append(path: u64) -> Self {
        Self::new(QTAPPEND, 0, path)
    }

    pub const fn is_dir(self) -> bool {
        self.qtype & QTDIR != 0
    }
}
