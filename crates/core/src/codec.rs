use crate::{
    error::{Error, Result},
    message::{
        RMessage, TMessage, MAXWELEM, RATTACH, RAUTH, RCLUNK, RCREATE, RERROR, RFLUSH, ROPEN,
        RREAD, RREMOVE, RSTAT, RVERSION, RWALK, RWRITE, RWSTAT, TATTACH, TAUTH, TCLUNK, TCREATE,
        TFLUSH, TOPEN, TREAD, TREMOVE, TSTAT, TVERSION, TWALK, TWRITE, TWSTAT,
    },
    stat::{push_qid, push_string, push_u16, push_u32, push_u64, Cursor, Stat},
};
use std::io::{Read, Write};

pub const FRAME_HEADER_SIZE: u32 = 7;
pub const RREAD_HEADER_SIZE: u32 = 11;
pub const TWRITE_HEADER_SIZE: u32 = 23;
pub const DEFAULT_MSIZE: u32 = 8192;
pub const MIN_MSIZE: u32 = 256;
pub const MAX_MSIZE: u32 = 64 * 1024;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Default)]
pub enum Variant {
    #[default]
    Plain,
}

impl Variant {
    pub const fn wire_name(self) -> &'static [u8] {
        match self {
            Variant::Plain => b"9P2000",
        }
    }

    pub fn accept(self, requested: &[u8]) -> Option<Variant> {
        if requested.starts_with(self.wire_name()) {
            Some(self)
        } else {
            None
        }
    }
}

pub fn clamp_read_count(msize: u32, requested: u32) -> u32 {
    requested.min(msize.saturating_sub(RREAD_HEADER_SIZE))
}

pub fn read_tmessage<R: Read>(reader: &mut R) -> Result<Option<TMessage>> {
    read_frame(reader)?
        .map(|frame| decode_tmessage(&frame))
        .transpose()
}

pub fn read_rmessage<R: Read>(reader: &mut R) -> Result<Option<RMessage>> {
    read_frame(reader)?
        .map(|frame| decode_rmessage(&frame))
        .transpose()
}

pub fn write_tmessage<W: Write>(writer: &mut W, message: &TMessage) -> Result<()> {
    let frame = encode_tmessage(message)?;
    writer
        .write_all(&frame)
        .and_then(|_| writer.flush())
        .map_err(|error| Error::from(format!("write 9P request frame: {error}")))
}

pub fn write_rmessage_checked<W: Write>(
    writer: &mut W,
    msize: u32,
    message: &RMessage,
) -> Result<()> {
    let frame = encode_rmessage_checked(message, msize)?;
    writer
        .write_all(&frame)
        .and_then(|_| writer.flush())
        .map_err(|error| Error::from(format!("write 9P response frame: {error}")))
}

fn read_frame<R: Read>(reader: &mut R) -> Result<Option<Vec<u8>>> {
    let mut prefix = [0_u8; 4];
    match reader.read_exact(&mut prefix) {
        Ok(()) => {}
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::BrokenPipe
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(Error::from(format!("read 9P frame size: {error}"))),
    }

    let size = u32::from_le_bytes(prefix);
    if size < FRAME_HEADER_SIZE {
        return Err(Error::from(format!("short 9P frame size {size}")));
    }
    let rest_len = usize::try_from(size - 4)
        .map_err(|_| Error::from(format!("oversized frame size {size}")))?;
    let mut frame = Vec::with_capacity(usize::try_from(size).unwrap_or(rest_len + 4));
    frame.extend(prefix);
    frame.resize(rest_len + 4, 0);
    reader
        .read_exact(&mut frame[4..])
        .map_err(|error| Error::from(format!("read 9P frame body: {error}")))?;
    Ok(Some(frame))
}

pub fn max_write_payload(msize: u32) -> u32 {
    msize.saturating_sub(TWRITE_HEADER_SIZE)
}

pub fn validate_frame_size(frame: &[u8]) -> Result<()> {
    if frame.len() < FRAME_HEADER_SIZE as usize {
        return Err(Error::from("truncated frame"));
    }
    let size = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]);
    let actual = u32::try_from(frame.len()).map_err(|_| Error::from("frame too large"))?;
    if size != actual {
        return Err(Error::from("frame size mismatch"));
    }
    Ok(())
}

pub fn decode_tmessage(frame: &[u8]) -> Result<TMessage> {
    validate_frame_size(frame)?;
    let message_type = frame[4];
    let tag = u16::from_le_bytes([frame[5], frame[6]]);
    let mut cursor = Cursor::new(&frame[FRAME_HEADER_SIZE as usize..]);
    let message = match message_type {
        TVERSION => TMessage::Version {
            tag,
            msize: cursor.u32()?,
            version: cursor.string()?,
        },
        TAUTH => TMessage::Auth {
            tag,
            afid: cursor.u32()?,
            uname: cursor.string()?,
            aname: cursor.string()?,
        },
        TATTACH => TMessage::Attach {
            tag,
            fid: cursor.u32()?,
            afid: cursor.u32()?,
            uname: cursor.string()?,
            aname: cursor.string()?,
        },
        TFLUSH => TMessage::Flush {
            tag,
            oldtag: cursor.u16()?,
        },
        TWALK => {
            let fid = cursor.u32()?;
            let newfid = cursor.u32()?;
            let nwname = usize::from(cursor.u16()?);
            if nwname > MAXWELEM {
                return Err(Error::from("name too long"));
            }
            let mut wnames = Vec::with_capacity(nwname);
            for _ in 0..nwname {
                wnames.push(cursor.string()?);
            }
            TMessage::Walk {
                tag,
                fid,
                newfid,
                wnames,
            }
        }
        TOPEN => TMessage::Open {
            tag,
            fid: cursor.u32()?,
            mode: cursor.u8()?,
        },
        TCREATE => TMessage::Create {
            tag,
            fid: cursor.u32()?,
            name: cursor.string()?,
            perm: cursor.u32()?,
            mode: cursor.u8()?,
        },
        TREAD => TMessage::Read {
            tag,
            fid: cursor.u32()?,
            offset: cursor.u64()?,
            count: cursor.u32()?,
        },
        TWRITE => {
            let fid = cursor.u32()?;
            let offset = cursor.u64()?;
            let count = cursor.u32()?;
            let data = cursor
                .bytes(usize::try_from(count).map_err(|_| Error::from("write count too large"))?)?;
            TMessage::Write {
                tag,
                fid,
                offset,
                data,
            }
        }
        TCLUNK => TMessage::Clunk {
            tag,
            fid: cursor.u32()?,
        },
        TREMOVE => TMessage::Remove {
            tag,
            fid: cursor.u32()?,
        },
        TSTAT => TMessage::Stat {
            tag,
            fid: cursor.u32()?,
        },
        TWSTAT => {
            let fid = cursor.u32()?;
            let nstat = usize::from(cursor.u16()?);
            let stat = Stat::decode(&cursor.bytes(nstat)?)?;
            TMessage::Wstat { tag, fid, stat }
        }
        _ => return Err(Error::from("unknown message type")),
    };
    if cursor.remaining() != 0 {
        return Err(Error::from("trailing frame bytes"));
    }
    Ok(message)
}

pub fn decode_rmessage(frame: &[u8]) -> Result<RMessage> {
    validate_frame_size(frame)?;
    let message_type = frame[4];
    let tag = u16::from_le_bytes([frame[5], frame[6]]);
    let mut cursor = Cursor::new(&frame[FRAME_HEADER_SIZE as usize..]);
    let message = match message_type {
        RVERSION => RMessage::Version {
            tag,
            msize: cursor.u32()?,
            version: cursor.string()?,
        },
        RAUTH => RMessage::Auth {
            tag,
            aqid: cursor.qid()?,
        },
        RATTACH => RMessage::Attach {
            tag,
            qid: cursor.qid()?,
        },
        RERROR => RMessage::Error {
            tag,
            ename: cursor.string()?,
        },
        RFLUSH => RMessage::Flush { tag },
        RWALK => {
            let nwqid = usize::from(cursor.u16()?);
            if nwqid > MAXWELEM {
                return Err(Error::from("name too long"));
            }
            let mut qids = Vec::with_capacity(nwqid);
            for _ in 0..nwqid {
                qids.push(cursor.qid()?);
            }
            RMessage::Walk { tag, qids }
        }
        ROPEN => RMessage::Open {
            tag,
            qid: cursor.qid()?,
            iounit: cursor.u32()?,
        },
        RCREATE => RMessage::Create {
            tag,
            qid: cursor.qid()?,
            iounit: cursor.u32()?,
        },
        RREAD => {
            let count = cursor.u32()?;
            let data = cursor
                .bytes(usize::try_from(count).map_err(|_| Error::from("read count too large"))?)?;
            RMessage::Read { tag, data }
        }
        RWRITE => RMessage::Write {
            tag,
            count: cursor.u32()?,
        },
        RCLUNK => RMessage::Clunk { tag },
        RREMOVE => RMessage::Remove { tag },
        RSTAT => {
            let nstat = usize::from(cursor.u16()?);
            let stat = Stat::decode(&cursor.bytes(nstat)?)?;
            RMessage::Stat { tag, stat }
        }
        RWSTAT => RMessage::Wstat { tag },
        _ => return Err(Error::from("unknown message type")),
    };
    if cursor.remaining() != 0 {
        return Err(Error::from("trailing frame bytes"));
    }
    Ok(message)
}

pub fn encode_tmessage(message: &TMessage) -> Result<Vec<u8>> {
    let mut frame = frame_prefix(message.message_type(), message.tag());
    match message {
        TMessage::Version { msize, version, .. } => {
            push_u32(&mut frame, *msize);
            push_string(&mut frame, version)?;
        }
        TMessage::Auth {
            afid, uname, aname, ..
        } => {
            push_u32(&mut frame, *afid);
            push_string(&mut frame, uname)?;
            push_string(&mut frame, aname)?;
        }
        TMessage::Attach {
            fid,
            afid,
            uname,
            aname,
            ..
        } => {
            push_u32(&mut frame, *fid);
            push_u32(&mut frame, *afid);
            push_string(&mut frame, uname)?;
            push_string(&mut frame, aname)?;
        }
        TMessage::Flush { oldtag, .. } => push_u16(&mut frame, *oldtag),
        TMessage::Walk {
            fid,
            newfid,
            wnames,
            ..
        } => {
            if wnames.len() > MAXWELEM {
                return Err(Error::from("name too long"));
            }
            push_u32(&mut frame, *fid);
            push_u32(&mut frame, *newfid);
            push_u16(
                &mut frame,
                u16::try_from(wnames.len()).map_err(|_| Error::from("name too long"))?,
            );
            for name in wnames {
                push_string(&mut frame, name)?;
            }
        }
        TMessage::Open { fid, mode, .. } => {
            push_u32(&mut frame, *fid);
            frame.push(*mode);
        }
        TMessage::Create {
            fid,
            name,
            perm,
            mode,
            ..
        } => {
            push_u32(&mut frame, *fid);
            push_string(&mut frame, name)?;
            push_u32(&mut frame, *perm);
            frame.push(*mode);
        }
        TMessage::Read {
            fid, offset, count, ..
        } => {
            push_u32(&mut frame, *fid);
            push_u64(&mut frame, *offset);
            push_u32(&mut frame, *count);
        }
        TMessage::Write {
            fid, offset, data, ..
        } => {
            let count = u32::try_from(data.len()).map_err(|_| Error::from("write too large"))?;
            push_u32(&mut frame, *fid);
            push_u64(&mut frame, *offset);
            push_u32(&mut frame, count);
            frame.extend(data);
        }
        TMessage::Clunk { fid, .. } | TMessage::Remove { fid, .. } | TMessage::Stat { fid, .. } => {
            push_u32(&mut frame, *fid)
        }
        TMessage::Wstat { fid, stat, .. } => {
            let encoded = stat.encode()?;
            push_u32(&mut frame, *fid);
            push_u16(
                &mut frame,
                u16::try_from(encoded.len()).map_err(|_| Error::from("stat too large"))?,
            );
            frame.extend(encoded);
        }
    }
    finish_frame(frame)
}

pub fn encode_rmessage(message: &RMessage) -> Result<Vec<u8>> {
    let mut frame = frame_prefix(message.message_type(), message.tag());
    match message {
        RMessage::Version { msize, version, .. } => {
            push_u32(&mut frame, *msize);
            push_string(&mut frame, version)?;
        }
        RMessage::Auth { aqid, .. } => push_qid(&mut frame, *aqid),
        RMessage::Attach { qid, .. } => push_qid(&mut frame, *qid),
        RMessage::Error { ename, .. } => push_string(&mut frame, ename)?,
        RMessage::Flush { .. }
        | RMessage::Clunk { .. }
        | RMessage::Remove { .. }
        | RMessage::Wstat { .. } => {}
        RMessage::Walk { qids, .. } => {
            if qids.len() > MAXWELEM {
                return Err(Error::from("name too long"));
            }
            push_u16(
                &mut frame,
                u16::try_from(qids.len()).map_err(|_| Error::from("name too long"))?,
            );
            for qid in qids {
                push_qid(&mut frame, *qid);
            }
        }
        RMessage::Open { qid, iounit, .. } | RMessage::Create { qid, iounit, .. } => {
            push_qid(&mut frame, *qid);
            push_u32(&mut frame, *iounit);
        }
        RMessage::Read { data, .. } => {
            let count = u32::try_from(data.len()).map_err(|_| Error::from("read too large"))?;
            push_u32(&mut frame, count);
            frame.extend(data);
        }
        RMessage::Write { count, .. } => push_u32(&mut frame, *count),
        RMessage::Stat { stat, .. } => {
            let encoded = stat.encode()?;
            push_u16(
                &mut frame,
                u16::try_from(encoded.len()).map_err(|_| Error::from("stat too large"))?,
            );
            frame.extend(encoded);
        }
    }
    finish_frame(frame)
}

pub fn encode_rmessage_checked(message: &RMessage, msize: u32) -> Result<Vec<u8>> {
    let frame = encode_rmessage(message)?;
    let len = u32::try_from(frame.len()).map_err(|_| Error::from("frame too large"))?;
    if len > msize {
        return Err(Error::from("frame exceeds msize"));
    }
    Ok(frame)
}

fn frame_prefix(message_type: u8, tag: u16) -> Vec<u8> {
    let mut frame = Vec::new();
    push_u32(&mut frame, 0);
    frame.push(message_type);
    push_u16(&mut frame, tag);
    frame
}

fn finish_frame(mut frame: Vec<u8>) -> Result<Vec<u8>> {
    let size = u32::try_from(frame.len()).map_err(|_| Error::from("frame too large"))?;
    frame[0..4].copy_from_slice(&size.to_le_bytes());
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        message::{RMessage, TMessage, MAXWELEM},
        qid::{Qid, DMDIR},
        stat::{dirread_chunk, Stat},
    };

    #[test]
    fn tmessage_round_trip_preserves_write_payload() -> Result<()> {
        let message = TMessage::Write {
            tag: 7,
            fid: 3,
            offset: 42,
            data: b"hello".to_vec(),
        };
        let frame = encode_tmessage(&message)?;
        assert_eq!(decode_tmessage(&frame)?, message);
        Ok(())
    }

    #[test]
    fn rmessage_round_trip_preserves_stat() -> Result<()> {
        let stat = Stat::new("body", Qid::file(9), 0o600);
        let message = RMessage::Stat {
            tag: 9,
            stat: stat.clone(),
        };
        let frame = encode_rmessage(&message)?;
        assert_eq!(decode_rmessage(&frame)?, RMessage::Stat { tag: 9, stat });
        Ok(())
    }

    #[test]
    fn dirread_chunk_never_splits_stat_entries() -> Result<()> {
        let first = Stat::new("short", Qid::file(1), 0o400);
        let second = Stat::new("very-long-name", Qid::file(2), 0o400);
        let first_len = first.encode()?.len();
        let count = u32::try_from(first_len + 1).map_err(|_| Error::from("bad test count"))?;
        let chunk = dirread_chunk(&[first.clone(), second], 0, count)?;
        assert_eq!(chunk, first.encode()?);
        Ok(())
    }

    #[test]
    fn stat_length_defaults_to_zero_for_compatibility() {
        let stat = Stat::new(".", Qid::dir(0), DMDIR | 0o500);
        assert_eq!(stat.length, 0);
    }

    #[test]
    fn malformed_frames_are_rejected_without_panicking() -> Result<()> {
        assert!(decode_tmessage(&[]).is_err());
        let mut bad_size = encode_tmessage(&TMessage::Read {
            tag: 1,
            fid: 1,
            offset: 0,
            count: u32::MAX,
        })?;
        bad_size[0] = 0;
        assert!(decode_tmessage(&bad_size).is_err());

        let oversized_walk = TMessage::Walk {
            tag: 2,
            fid: 1,
            newfid: 2,
            wnames: (0..=MAXWELEM).map(|_| b"x".to_vec()).collect(),
        };
        assert!(encode_tmessage(&oversized_walk).is_err());
        Ok(())
    }

    #[test]
    fn read_and_write_counts_obey_msize_helpers() {
        assert_eq!(clamp_read_count(8192, u32::MAX), 8192 - RREAD_HEADER_SIZE);
        assert_eq!(max_write_payload(8192), 8192 - TWRITE_HEADER_SIZE);
    }
}
