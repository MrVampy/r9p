use crate::{
    error::{Error, Result},
    qid::Qid,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stat {
    pub type_: u16,
    pub dev: u32,
    pub qid: Qid,
    pub mode: u32,
    pub atime: u32,
    pub mtime: u32,
    pub length: u64,
    pub name: Vec<u8>,
    pub uid: Vec<u8>,
    pub gid: Vec<u8>,
    pub muid: Vec<u8>,
}

impl Stat {
    pub fn new(name: impl Into<Vec<u8>>, qid: Qid, mode: u32) -> Self {
        Self {
            type_: 0,
            dev: 0,
            qid,
            mode,
            atime: 0,
            mtime: 0,
            length: 0,
            name: name.into(),
            uid: b"racme".to_vec(),
            gid: b"racme".to_vec(),
            muid: b"racme".to_vec(),
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut body = Vec::new();
        push_u16(&mut body, self.type_);
        push_u32(&mut body, self.dev);
        push_qid(&mut body, self.qid);
        push_u32(&mut body, self.mode);
        push_u32(&mut body, self.atime);
        push_u32(&mut body, self.mtime);
        push_u64(&mut body, self.length);
        push_string(&mut body, &self.name)?;
        push_string(&mut body, &self.uid)?;
        push_string(&mut body, &self.gid)?;
        push_string(&mut body, &self.muid)?;

        let size = u16::try_from(body.len()).map_err(|_| Error::from("stat too large"))?;
        let mut encoded = Vec::with_capacity(body.len() + 2);
        push_u16(&mut encoded, size);
        encoded.extend(body);
        Ok(encoded)
    }

    pub fn decode(encoded: &[u8]) -> Result<Self> {
        let mut cursor = Cursor::new(encoded);
        let size = cursor.u16()? as usize;
        if size != cursor.remaining() {
            return Err(Error::from("stat size mismatch"));
        }
        let type_ = cursor.u16()?;
        let dev = cursor.u32()?;
        let qid = cursor.qid()?;
        let mode = cursor.u32()?;
        let atime = cursor.u32()?;
        let mtime = cursor.u32()?;
        let length = cursor.u64()?;
        let name = cursor.string()?;
        let uid = cursor.string()?;
        let gid = cursor.string()?;
        let muid = cursor.string()?;
        if cursor.remaining() != 0 {
            return Err(Error::from("trailing stat bytes"));
        }
        Ok(Self {
            type_,
            dev,
            qid,
            mode,
            atime,
            mtime,
            length,
            name,
            uid,
            gid,
            muid,
        })
    }
}

pub fn dirread_chunk(stats: &[Stat], offset: u64, count: u32) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut position = 0_u64;
    let limit = usize::try_from(count).map_err(|_| Error::from("count too large"))?;
    for stat in stats {
        let encoded = stat.encode()?;
        let len = u64::try_from(encoded.len()).map_err(|_| Error::from("stat too large"))?;
        let next = position.saturating_add(len);
        if next <= offset {
            position = next;
            continue;
        }
        if position < offset {
            break;
        }
        if out.len().saturating_add(encoded.len()) > limit {
            break;
        }
        out.extend(encoded);
        position = next;
    }
    Ok(out)
}

pub fn decode_dir_entries(data: &[u8]) -> Result<Vec<Stat>> {
    let mut entries = Vec::new();
    let mut offset = 0_usize;
    while offset < data.len() {
        if data.len().saturating_sub(offset) < 2 {
            return Err(Error::from("truncated directory stat"));
        }
        let size = u16::from_le_bytes([data[offset], data[offset + 1]]) as usize;
        let end = offset
            .checked_add(size + 2)
            .ok_or_else(|| Error::from("directory stat overflow"))?;
        let entry = data
            .get(offset..end)
            .ok_or_else(|| Error::from("truncated directory stat"))?;
        entries.push(Stat::decode(entry)?);
        offset = end;
    }
    Ok(entries)
}

pub(crate) fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend(value.to_le_bytes());
}

pub(crate) fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend(value.to_le_bytes());
}

pub(crate) fn push_u64(out: &mut Vec<u8>, value: u64) {
    out.extend(value.to_le_bytes());
}

pub(crate) fn push_qid(out: &mut Vec<u8>, qid: Qid) {
    out.push(qid.qtype);
    push_u32(out, qid.version);
    push_u64(out, qid.path);
}

pub(crate) fn push_string(out: &mut Vec<u8>, value: &[u8]) -> Result<()> {
    let len = u16::try_from(value.len()).map_err(|_| Error::from("string too large"))?;
    push_u16(out, len);
    out.extend(value);
    Ok(())
}

pub(crate) struct Cursor<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    pub(crate) const fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    pub(crate) fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.position)
    }

    pub(crate) fn u8(&mut self) -> Result<u8> {
        let bytes = self.take(1)?;
        Ok(bytes[0])
    }

    pub(crate) fn u16(&mut self) -> Result<u16> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    pub(crate) fn u32(&mut self) -> Result<u32> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub(crate) fn u64(&mut self) -> Result<u64> {
        let bytes = self.take(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    pub(crate) fn qid(&mut self) -> Result<Qid> {
        let qtype = self.u8()?;
        let version = self.u32()?;
        let path = self.u64()?;
        Ok(Qid::new(qtype, version, path))
    }

    pub(crate) fn string(&mut self) -> Result<Vec<u8>> {
        let len = usize::from(self.u16()?);
        Ok(self.take(len)?.to_vec())
    }

    pub(crate) fn bytes(&mut self, len: usize) -> Result<Vec<u8>> {
        Ok(self.take(len)?.to_vec())
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(len)
            .ok_or_else(|| Error::from("frame offset overflow"))?;
        let bytes = self
            .data
            .get(self.position..end)
            .ok_or_else(|| Error::from("truncated frame"))?;
        self.position = end;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::{decode_dir_entries, Stat};
    use crate::{error::Result, qid::Qid};

    #[test]
    fn directory_entry_stream_decodes_concatenated_stats() -> Result<()> {
        let first = Stat::new("first", Qid::file(1), 0o400);
        let second = Stat::new("second", Qid::file(2), 0o600);
        let mut data = first.encode()?;
        data.extend(second.encode()?);
        assert_eq!(decode_dir_entries(&data)?, vec![first, second]);
        Ok(())
    }

    #[test]
    fn directory_entry_stream_rejects_truncated_stats() {
        assert!(decode_dir_entries(&[10, 0, 1]).is_err());
    }
}
