use crate::{Client, Error as SessionError};
use r9p::{
    error::{Error, Result, ENOAUTH},
    fid::{Fid, NOFID},
    server::{
        ConnectionHandler, OpenFile, ReadData, ServerCompletion, ServerRequest, ServerRequestKind,
    },
};
use std::{
    collections::BTreeMap,
    sync::{atomic::AtomicBool, Mutex},
    time::Duration,
};

pub(super) struct ProjectionHandler {
    client: Client,
    source: String,
    operation_timeout: Duration,
    fids: Mutex<BTreeMap<Fid, Fid>>,
}

impl ProjectionHandler {
    pub(super) fn new(client: Client, source: String, operation_timeout: Duration) -> Self {
        Self {
            client,
            source,
            operation_timeout,
            fids: Mutex::new(BTreeMap::new()),
        }
    }

    fn remote_fid(&self, fid: Fid) -> Result<Fid> {
        self.fids
            .lock()
            .map_err(|_| Error::from_static("namespace projection fid map poisoned"))?
            .get(&fid)
            .copied()
            .ok_or_else(|| Error::from_static("unknown projected fid"))
    }

    fn insert_fid(&self, fid: Fid, remote: Fid) -> Result<Option<Fid>> {
        self.fids
            .lock()
            .map_err(|_| Error::from_static("namespace projection fid map poisoned"))
            .map(|mut fids| fids.insert(fid, remote))
    }

    fn remove_fid(&self, fid: Fid) -> Result<Fid> {
        self.fids
            .lock()
            .map_err(|_| Error::from_static("namespace projection fid map poisoned"))?
            .remove(&fid)
            .ok_or_else(|| Error::from_static("unknown projected fid"))
    }

    fn attach(&self, fid: Fid, afid: Fid, aname: &[u8]) -> Result<ServerCompletion> {
        if afid != NOFID {
            return Err(Error::from_static(ENOAUTH));
        }
        if !aname.is_empty() && aname != b"/" {
            return Err(Error::from_static("projected namespace has a fixed root"));
        }
        let remote = self
            .client
            .walk_path_timeout(&self.source, self.operation_timeout)
            .map_err(projected_error)?;
        let stat = match self.client.stat_timeout(remote, self.operation_timeout) {
            Ok(stat) => stat,
            Err(error) => {
                let _ = self.client.clunk_timeout(remote, self.operation_timeout);
                return Err(projected_error(error));
            }
        };
        if let Some(previous) = self.insert_fid(fid, remote)? {
            let _ = self.client.clunk_timeout(previous, self.operation_timeout);
        }
        Ok(ServerCompletion::Attach { qid: stat.qid })
    }

    fn walk(&self, fid: Fid, newfid: Fid, names: &[Vec<u8>]) -> Result<ServerCompletion> {
        let source = self.remote_fid(fid)?;
        if names.is_empty() {
            if newfid != fid {
                let remote = self
                    .client
                    .clone_fid_timeout(source, self.operation_timeout)
                    .map_err(projected_error)?;
                if let Some(previous) = self.insert_fid(newfid, remote)? {
                    let _ = self.client.clunk_timeout(previous, self.operation_timeout);
                }
            }
            return Ok(ServerCompletion::Walk { qids: Vec::new() });
        }

        let mut current = source;
        let mut current_is_temporary = false;
        let mut qids = Vec::with_capacity(names.len());
        for name in names {
            let next = match self
                .client
                .walk_one_timeout(current, name, self.operation_timeout)
            {
                Ok(next) => next,
                Err(error) if qids.is_empty() => return Err(projected_error(error)),
                Err(_) => {
                    if current_is_temporary {
                        let _ = self.client.clunk_timeout(current, self.operation_timeout);
                    }
                    return Ok(ServerCompletion::Walk { qids });
                }
            };
            let stat = match self.client.stat_timeout(next, self.operation_timeout) {
                Ok(stat) => stat,
                Err(error) => {
                    let _ = self.client.clunk_timeout(next, self.operation_timeout);
                    if current_is_temporary {
                        let _ = self.client.clunk_timeout(current, self.operation_timeout);
                    }
                    return Err(projected_error(error));
                }
            };
            if current_is_temporary {
                let _ = self.client.clunk_timeout(current, self.operation_timeout);
            }
            current = next;
            current_is_temporary = true;
            qids.push(stat.qid);
        }

        let previous = self.insert_fid(newfid, current)?;
        if let Some(previous) = previous {
            if previous != current {
                let _ = self.client.clunk_timeout(previous, self.operation_timeout);
            }
        }
        Ok(ServerCompletion::Walk { qids })
    }

    fn open(&self, fid: Fid, mode: u8) -> Result<ServerCompletion> {
        let qid = self
            .client
            .open_timeout(self.remote_fid(fid)?, mode, self.operation_timeout)
            .map_err(projected_error)?;
        Ok(ServerCompletion::Open(OpenFile { qid, iounit: 0 }))
    }

    fn create(&self, fid: Fid, name: &[u8], perm: u32, mode: u8) -> Result<ServerCompletion> {
        let parent = self.remote_fid(fid)?;
        let (created, qid) = self
            .client
            .create_timeout(parent, name, perm, mode, self.operation_timeout)
            .map_err(projected_error)?;
        if let Some(previous) = self.insert_fid(fid, created)? {
            let _ = self.client.clunk_timeout(previous, self.operation_timeout);
        }
        Ok(ServerCompletion::Create(OpenFile { qid, iounit: 0 }))
    }

    fn read(&self, fid: Fid, offset: u64, count: u32) -> Result<ServerCompletion> {
        self.client
            .read(self.remote_fid(fid)?, offset, count)
            .map(ReadData::Bytes)
            .map(ServerCompletion::Read)
            .map_err(projected_error)
    }

    fn write(&self, fid: Fid, offset: u64, data: &[u8]) -> Result<ServerCompletion> {
        self.client
            .write_timeout(self.remote_fid(fid)?, offset, data, self.operation_timeout)
            .map(|count| ServerCompletion::Write { count })
            .map_err(projected_error)
    }

    fn clunk(&self, fid: Fid) -> Result<ServerCompletion> {
        let remote = self.remote_fid(fid)?;
        self.client
            .clunk_timeout(remote, self.operation_timeout)
            .map_err(projected_error)?;
        self.remove_fid(fid)?;
        Ok(ServerCompletion::Clunk)
    }

    fn remove(&self, fid: Fid) -> Result<ServerCompletion> {
        let remote = self.remote_fid(fid)?;
        self.client
            .remove_timeout(remote, self.operation_timeout)
            .map_err(projected_error)?;
        self.remove_fid(fid)?;
        Ok(ServerCompletion::Remove)
    }

    fn stat(&self, fid: Fid) -> Result<ServerCompletion> {
        self.client
            .stat_timeout(self.remote_fid(fid)?, self.operation_timeout)
            .map(|stat| ServerCompletion::Stat { stat })
            .map_err(projected_error)
    }

    fn wstat(&self, fid: Fid, stat: r9p::stat::Stat) -> Result<ServerCompletion> {
        self.client
            .wstat_timeout(self.remote_fid(fid)?, stat, self.operation_timeout)
            .map(|()| ServerCompletion::Wstat)
            .map_err(projected_error)
    }

    fn rename_at(
        &self,
        olddirfid: Fid,
        oldname: &[u8],
        newdirfid: Fid,
        newname: &[u8],
    ) -> Result<ServerCompletion> {
        self.client
            .rename_at_timeout(
                self.remote_fid(olddirfid)?,
                oldname,
                self.remote_fid(newdirfid)?,
                newname,
                self.operation_timeout,
            )
            .map(|()| ServerCompletion::RenameAt)
            .map_err(projected_error)
    }
}

impl ConnectionHandler for ProjectionHandler {
    fn perform(
        &self,
        request: &ServerRequest,
        _cancel: Option<&AtomicBool>,
    ) -> Result<ServerCompletion> {
        match &request.kind {
            ServerRequestKind::Auth { .. } => Err(Error::from_static(ENOAUTH)),
            ServerRequestKind::Attach {
                fid, afid, aname, ..
            } => self.attach(*fid, *afid, aname),
            ServerRequestKind::Walk {
                fid,
                newfid,
                wnames,
                ..
            } => self.walk(*fid, *newfid, wnames),
            ServerRequestKind::Open { fid, mode, .. } => self.open(*fid, *mode),
            ServerRequestKind::Create {
                fid,
                name,
                perm,
                mode,
                ..
            } => self.create(*fid, name, *perm, *mode),
            ServerRequestKind::Read {
                fid, offset, count, ..
            } => self.read(*fid, *offset, *count),
            ServerRequestKind::Write {
                fid, offset, data, ..
            } => self.write(*fid, *offset, data),
            ServerRequestKind::Clunk { fid, .. } => self.clunk(*fid),
            ServerRequestKind::Remove { fid, .. } => self.remove(*fid),
            ServerRequestKind::Stat { fid, .. } => self.stat(*fid),
            ServerRequestKind::Wstat { fid, stat, .. } => self.wstat(*fid, stat.clone()),
            ServerRequestKind::RenameAt {
                olddirfid,
                oldname,
                newdirfid,
                newname,
                ..
            } => self.rename_at(*olddirfid, oldname, *newdirfid, newname),
            ServerRequestKind::Referrals { .. } => Ok(ServerCompletion::Referrals {
                referrals: Vec::new(),
            }),
        }
    }

    fn is_async(&self, request: &ServerRequest) -> bool {
        matches!(request.kind, ServerRequestKind::Read { .. })
    }

    fn cancellation_fid(&self, request: &ServerRequest) -> Option<Fid> {
        match request.kind {
            ServerRequestKind::Read { fid, .. } => Some(fid),
            _ => None,
        }
    }

    fn reset(&self) -> Result<()> {
        let remotes = self
            .fids
            .lock()
            .map_err(|_| Error::from_static("namespace projection fid map poisoned"))
            .map(|mut fids| std::mem::take(&mut *fids))?;
        for remote in remotes.into_values() {
            let _ = self.client.clunk_timeout(remote, self.operation_timeout);
        }
        Ok(())
    }

    fn wake_after_cancel(&self) {
        let _ = self.client.shutdown();
    }
}

fn projected_error(error: SessionError) -> Error {
    Error::from(error.message().to_string())
}
