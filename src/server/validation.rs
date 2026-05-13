use crate::{
    error::{Error, Result, EBADTAG, EBADWNAME},
    message::{RMessage, Tag, MAXWELEM},
};

pub fn validate_walk_names(wnames: &[Vec<u8>]) -> Result<()> {
    if wnames.len() > MAXWELEM {
        return Err(Error::from("name too long"));
    }
    for name in wnames {
        if name.is_empty()
            || name.contains(&b'/')
            || name.contains(&0)
            || name.len() > u8::MAX as usize
        {
            return Err(Error::from_static(EBADWNAME));
        }
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
