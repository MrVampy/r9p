use crate::{Client, Error, Result};
use std::sync::{Arc, RwLock};

#[derive(Clone)]
pub struct ClientSlot {
    current: Arc<RwLock<Client>>,
}

impl ClientSlot {
    pub fn new(client: Client) -> Self {
        Self {
            current: Arc::new(RwLock::new(client)),
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
        Ok(())
    }
}
