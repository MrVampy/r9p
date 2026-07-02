use std::{
    sync::{Arc, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{Error, Result};

#[derive(Clone, Debug)]
pub struct SessionEpoch {
    current: Arc<RwLock<String>>,
}

impl SessionEpoch {
    pub fn new() -> Self {
        Self {
            current: Arc::new(RwLock::new(new_epoch_value())),
        }
    }

    pub fn current(&self) -> Result<String> {
        self.current
            .read()
            .map_err(|_| Error::new(libc::EIO, "session epoch lock poisoned"))
            .map(|epoch| epoch.clone())
    }

    pub fn bump(&self) -> Result<String> {
        let next = new_epoch_value();
        let mut current = self
            .current
            .write()
            .map_err(|_| Error::new(libc::EIO, "session epoch lock poisoned"))?;
        *current = next.clone();
        Ok(next)
    }
}

impl Default for SessionEpoch {
    fn default() -> Self {
        Self::new()
    }
}

fn new_epoch_value() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("session:{}:{nanos}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::SessionEpoch;

    #[test]
    fn bump_changes_epoch_value() {
        let epoch = SessionEpoch::new();
        let first = epoch.current().expect("initial epoch");

        let second = epoch.bump().expect("bumped epoch");

        assert_ne!(first, second);
        assert_eq!(epoch.current().expect("current epoch"), second);
    }
}
