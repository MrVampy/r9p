use crate::{
    codec::Variant,
    error::{
        Error, Result, EBADTAG, EBADWNAME, EWSTATATIME, EWSTATDEV, EWSTATDIRLENGTH, EWSTATDMDIR,
        EWSTATDMSYMLINK, EWSTATMUID, EWSTATQID, EWSTATTYPE, EWSTATUID,
    },
    message::{RMessage, Tag, MAXWELEM},
    qid::{Qid, DMDIR, DMSYMLINK},
    stat::Stat,
};

pub fn validate_walk_names(wnames: &[Vec<u8>]) -> Result<()> {
    if wnames.len() > MAXWELEM {
        return Err(Error::from("name too long"));
    }
    for name in wnames {
        if name.is_empty()
            || name.contains(&b'/')
            || name.contains(&0)
            || name.len() > u16::MAX as usize
        {
            return Err(Error::from_static(EBADWNAME));
        }
    }
    Ok(())
}

pub(super) fn validate_wstat(qid: Qid, stat: &Stat, variant: Variant) -> Result<()> {
    if stat.type_ != u16::MAX {
        return Err(Error::from_static(EWSTATTYPE));
    }
    if stat.dev != u32::MAX {
        return Err(Error::from_static(EWSTATDEV));
    }
    if stat.qid != Qid::new(u8::MAX, u32::MAX, u64::MAX) {
        return Err(Error::from_static(EWSTATQID));
    }
    if stat.atime != u32::MAX {
        return Err(Error::from_static(EWSTATATIME));
    }
    if !stat.uid.is_empty() {
        return Err(Error::from_static(EWSTATUID));
    }
    if !stat.muid.is_empty() {
        return Err(Error::from_static(EWSTATMUID));
    }
    if stat.mode != u32::MAX && ((stat.mode & DMDIR != 0) != qid.is_dir()) {
        return Err(Error::from_static(EWSTATDMDIR));
    }
    if stat.mode != u32::MAX {
        let requested_symlink = stat.mode & DMSYMLINK != 0;
        if requested_symlink != qid.is_symlink()
            || (requested_symlink && !variant.supports_symlinks())
        {
            return Err(Error::from_static(EWSTATDMSYMLINK));
        }
    }
    if qid.is_dir() && stat.length != u64::MAX && stat.length != 0 {
        return Err(Error::from_static(EWSTATDIRLENGTH));
    }
    Ok(())
}

pub(super) fn take_count(mut bytes: Vec<u8>, count: u32) -> Result<Vec<u8>> {
    let limit = usize::try_from(count).map_err(|_| Error::from("count too large"))?;
    if bytes.len() > limit {
        bytes.truncate(limit);
    }
    Ok(bytes)
}

pub fn error_reply(tag: Tag, error: Error) -> RMessage {
    let ename = if tag == crate::message::NOTAG && error.message() == EBADTAG.as_bytes() {
        EBADTAG.as_bytes().to_vec()
    } else {
        error.into_message()
    };
    RMessage::Error { tag, ename }
}
