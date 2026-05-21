//! `statfs` / `poll` op handlers.

use crate::{
    error::Result,
    fuse::{
        reply::{read_struct, reply_struct},
        wire::{FuseInHeader, FuseKstatfs, FusePollIn, FusePollOut, FuseStatfsOut},
        R9pFuse,
    },
};
use std::fs::File;

impl R9pFuse {
    pub(in crate::fuse) fn statfs(&mut self, file: &mut File, header: FuseInHeader) -> Result<()> {
        let out = FuseStatfsOut {
            st: FuseKstatfs {
                blocks: 0,
                bfree: 0,
                bavail: 0,
                files: 0,
                ffree: 0,
                bsize: 8192,
                namelen: 255,
                frsize: 8192,
                padding: 0,
                spare: [0; 6],
            },
        };
        reply_struct(file, header.unique, &out)
    }

    pub(in crate::fuse) fn poll(
        &mut self,
        file: &mut File,
        header: FuseInHeader,
        payload: &[u8],
    ) -> Result<()> {
        let input = read_struct::<FusePollIn>(payload)?;
        let out = FusePollOut {
            revents: input.events,
            padding: 0,
        };
        reply_struct(file, header.unique, &out)
    }
}
