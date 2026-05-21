//! Advisory lock handlers.

use crate::{
    error::Result,
    fuse::{
        reply::{read_struct, reply_empty, reply_struct},
        wire::{FuseFileLock, FuseInHeader, FuseLkIn, FuseLkOut},
        R9pFuse,
    },
};
use std::fs::File;

impl R9pFuse {
    pub(in crate::fuse) fn getlk(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
        payload: &[u8],
    ) -> Result<()> {
        let input = read_struct::<FuseLkIn>(payload)?;
        reply_struct(
            file,
            header.unique,
            &FuseLkOut {
                lk: unlocked_file_lock(input.lk),
            },
        )
    }

    pub(in crate::fuse) fn setlk(&mut self, file: &mut File, header: FuseInHeader) -> Result<()> {
        reply_empty(file, header.unique)
    }
}

fn unlocked_file_lock(lock: FuseFileLock) -> FuseFileLock {
    FuseFileLock {
        type_: libc::F_UNLCK as u32,
        ..lock
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlocked_lock_preserves_range_and_marks_unlocked() {
        let lock = FuseFileLock {
            start: 10,
            end: 20,
            type_: libc::F_WRLCK as u32,
            pid: 99,
        };

        assert_eq!(
            unlocked_file_lock(lock),
            FuseFileLock {
                start: 10,
                end: 20,
                type_: libc::F_UNLCK as u32,
                pid: 99,
            }
        );
    }
}
