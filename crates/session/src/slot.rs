use crate::{Client, Error, Result, SessionEpoch};
use std::sync::{Arc, RwLock};

#[derive(Clone)]
pub struct ClientSlot {
    current: Arc<RwLock<Client>>,
    epoch: SessionEpoch,
}

impl ClientSlot {
    pub fn new(client: Client) -> Self {
        Self::new_with_epoch(client, SessionEpoch::new())
    }

    pub fn new_with_epoch(client: Client, epoch: SessionEpoch) -> Self {
        Self {
            current: Arc::new(RwLock::new(client)),
            epoch,
        }
    }

    pub fn snapshot(&self) -> Result<Client> {
        self.current
            .read()
            .map_err(|_| Error::new(libc::EIO, "9P client lock poisoned"))
            .map(|client| client.clone())
    }

    pub fn replace(&self, client: Client) -> Result<()> {
        let mut current = self
            .current
            .write()
            .map_err(|_| Error::new(libc::EIO, "9P client lock poisoned"))?;
        *current = client;
        self.epoch.bump()?;
        Ok(())
    }

    pub fn session_epoch(&self) -> Result<String> {
        self.epoch.current()
    }
}
