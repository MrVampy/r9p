use super::*;

impl Client {
    pub fn open(&self, fid: Fid, mode: u8) -> Result<Qid> {
        let binding = self.binding(fid)?;
        match binding.target {
            FidTarget::ReferralDirectory if mode == r9p::OREAD => {
                Ok(referral_directory::stat(&binding.namespace_path).qid)
            }
            FidTarget::ReferralDirectory => Err(referral_directory::read_only_error()),
            FidTarget::Remote(remote) => remote.client.open(remote.fid, mode),
        }
    }

    pub fn open_timeout(&self, fid: Fid, mode: u8, timeout: Duration) -> Result<Qid> {
        let binding = self.binding(fid)?;
        match binding.target {
            FidTarget::ReferralDirectory if mode == r9p::OREAD => {
                Ok(referral_directory::stat(&binding.namespace_path).qid)
            }
            FidTarget::ReferralDirectory => Err(referral_directory::read_only_error()),
            FidTarget::Remote(remote) => remote.client.open_timeout(remote.fid, mode, timeout),
        }
    }

    pub fn create_timeout(
        &self,
        parent_fid: Fid,
        name: &[u8],
        perm: u32,
        mode: u8,
        timeout: Duration,
    ) -> Result<(Fid, Qid)> {
        validate_path_element(name)?;
        let parent = self.binding(parent_fid)?;
        let remote = parent.writable_remote()?;
        let mut namespace_path = parent.namespace_path.clone();
        namespace_path.push(name.to_vec());
        let selected = self.routed_target(&namespace_path, timeout)?;
        if selected.route_mount != remote.route_mount {
            return Err(Error::new(
                libc::EXDEV,
                "create cannot cross a namespace referral boundary",
            ));
        }
        let (remote_fid, qid) = remote
            .client
            .create_timeout(remote.fid, name, perm, mode, timeout)?;
        let fid = self.allocate_binding(FidBinding {
            namespace_path,
            target: FidTarget::Remote(RemoteFid {
                client: remote.client,
                fid: remote_fid,
                route_mount: remote.route_mount,
            }),
        })?;
        Ok((fid, qid))
    }

    pub fn create(&self, parent_fid: Fid, name: &[u8], perm: u32, mode: u8) -> Result<(Fid, Qid)> {
        validate_path_element(name)?;
        let parent = self.binding(parent_fid)?;
        let remote = parent.writable_remote()?;
        let mut namespace_path = parent.namespace_path.clone();
        namespace_path.push(name.to_vec());
        let selected = self.routed_target(&namespace_path, Duration::ZERO)?;
        if selected.route_mount != remote.route_mount {
            return Err(Error::new(
                libc::EXDEV,
                "create cannot cross a namespace referral boundary",
            ));
        }
        let (remote_fid, qid) = remote.client.create(remote.fid, name, perm, mode)?;
        let fid = self.allocate_binding(FidBinding {
            namespace_path,
            target: FidTarget::Remote(RemoteFid {
                client: remote.client,
                fid: remote_fid,
                route_mount: remote.route_mount,
            }),
        })?;
        Ok((fid, qid))
    }

    pub fn read_timeout(
        &self,
        fid: Fid,
        offset: u64,
        count: u32,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        let binding = self.binding(fid)?;
        self.read_binding(&binding, offset, count, |remote| {
            remote
                .client
                .read_timeout(remote.fid, offset, count, timeout)
        })
    }

    pub(crate) fn submit_read(&self, fid: Fid, offset: u64, count: u32) -> Result<PendingRead> {
        let binding = self.binding(fid)?;
        let remote = binding.remote()?;
        let pending = remote.client.submit_read(remote.fid, offset, count)?;
        Ok(PendingRead {
            client: remote.client,
            pending,
        })
    }

    pub(crate) fn flush_read_tag_timeout(
        &self,
        fid: Fid,
        tag: Tag,
        timeout: Duration,
    ) -> Result<()> {
        let binding = self.binding(fid)?;
        binding.remote()?.client.flush_tag_timeout(tag, timeout)
    }

    pub fn read(&self, fid: Fid, offset: u64, count: u32) -> Result<Vec<u8>> {
        let binding = self.binding(fid)?;
        self.read_binding(&binding, offset, count, |remote| {
            remote.client.read(remote.fid, offset, count)
        })
    }

    pub fn read_full_timeout(
        &self,
        fid: Fid,
        offset: u64,
        count: u32,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        let binding = self.binding(fid)?;
        self.read_binding(&binding, offset, count, |remote| {
            remote
                .client
                .read_full_timeout(remote.fid, offset, count, timeout)
        })
    }

    pub fn read_delimited_timeout(
        &self,
        fid: Fid,
        offset: u64,
        count: u32,
        delimiter: u8,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        let binding = self.binding(fid)?;
        let remote = binding.remote()?;
        remote
            .client
            .read_delimited_timeout(remote.fid, offset, count, delimiter, timeout)
    }

    pub fn read_full(&self, fid: Fid, offset: u64, count: u32) -> Result<Vec<u8>> {
        let binding = self.binding(fid)?;
        self.read_binding(&binding, offset, count, |remote| {
            remote.client.read_full(remote.fid, offset, count)
        })
    }

    pub fn read_delimited(
        &self,
        fid: Fid,
        offset: u64,
        count: u32,
        delimiter: u8,
    ) -> Result<Vec<u8>> {
        let binding = self.binding(fid)?;
        let remote = binding.remote()?;
        remote
            .client
            .read_delimited(remote.fid, offset, count, delimiter)
    }

    pub fn write_timeout(
        &self,
        fid: Fid,
        offset: u64,
        data: &[u8],
        timeout: Duration,
    ) -> Result<u32> {
        let remote = self.binding(fid)?.writable_remote()?;
        remote
            .client
            .write_timeout(remote.fid, offset, data, timeout)
    }

    pub fn write(&self, fid: Fid, offset: u64, data: &[u8]) -> Result<u32> {
        let remote = self.binding(fid)?.writable_remote()?;
        remote.client.write(remote.fid, offset, data)
    }

    pub fn write_once(&self, fid: Fid, offset: u64, data: &[u8]) -> Result<u32> {
        let remote = self.binding(fid)?.writable_remote()?;
        remote.client.write_once(remote.fid, offset, data)
    }

    pub fn write_once_timeout(
        &self,
        fid: Fid,
        offset: u64,
        data: &[u8],
        timeout: Duration,
    ) -> Result<u32> {
        let remote = self.binding(fid)?.writable_remote()?;
        remote
            .client
            .write_once_timeout(remote.fid, offset, data, timeout)
    }

    pub fn write_then_read_delimited_timeout(
        &self,
        fid: Fid,
        write_offset: u64,
        data: &[u8],
        read: DelimitedRead,
        timeout: Duration,
    ) -> std::result::Result<(u32, Vec<u8>), WriteThenReadError> {
        let binding = self.binding(fid).map_err(WriteThenReadError::Rejected)?;
        let remote = binding
            .writable_remote()
            .map_err(WriteThenReadError::Rejected)?;
        remote.client.write_then_read_delimited_timeout(
            remote.fid,
            write_offset,
            data,
            read,
            timeout,
        )
    }

    pub fn clunk_timeout(&self, fid: Fid, timeout: Duration) -> Result<()> {
        if fid == ROOT_FID {
            return Err(Error::new(libc::EBUSY, "cannot clunk the namespace root"));
        }
        let binding = self.binding(fid)?;
        if let FidTarget::Remote(remote) = binding.target {
            remote.client.clunk_timeout(remote.fid, timeout)?;
        }
        self.remove_binding(fid)
    }

    pub fn clunk(&self, fid: Fid) -> Result<()> {
        if fid == ROOT_FID {
            return Err(Error::new(libc::EBUSY, "cannot clunk the namespace root"));
        }
        let binding = self.binding(fid)?;
        if let FidTarget::Remote(remote) = binding.target {
            remote.client.clunk(remote.fid)?;
        }
        self.remove_binding(fid)
    }

    pub fn shutdown(&self) -> Result<()> {
        let routes = self
            .state
            .connected_routes
            .lock()
            .map_err(|_| Error::new(libc::EIO, "namespace route lock poisoned"))?
            .clone();
        let mut first_error = None;
        for route in routes {
            if let Err(error) = route.shutdown() {
                first_error.get_or_insert(error);
            }
        }
        if let Err(error) = self.state.root.shutdown() {
            first_error.get_or_insert(error);
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub fn remove_timeout(&self, fid: Fid, timeout: Duration) -> Result<()> {
        if fid == ROOT_FID {
            return Err(Error::new(libc::EBUSY, "cannot remove the namespace root"));
        }
        let remote = self.binding(fid)?.writable_remote()?;
        remote.client.remove_timeout(remote.fid, timeout)?;
        self.remove_binding(fid)
    }

    pub fn remove(&self, fid: Fid) -> Result<()> {
        if fid == ROOT_FID {
            return Err(Error::new(libc::EBUSY, "cannot remove the namespace root"));
        }
        let remote = self.binding(fid)?.writable_remote()?;
        remote.client.remove(remote.fid)?;
        self.remove_binding(fid)
    }

    pub fn stat(&self, fid: Fid) -> Result<Stat> {
        let binding = self.binding(fid)?;
        match binding.target {
            FidTarget::ReferralDirectory => Ok(referral_directory::stat(&binding.namespace_path)),
            FidTarget::Remote(remote) => remote.client.stat(remote.fid),
        }
    }

    pub fn stat_timeout(&self, fid: Fid, timeout: Duration) -> Result<Stat> {
        let binding = self.binding(fid)?;
        match binding.target {
            FidTarget::ReferralDirectory => Ok(referral_directory::stat(&binding.namespace_path)),
            FidTarget::Remote(remote) => remote.client.stat_timeout(remote.fid, timeout),
        }
    }

    pub fn wstat_timeout(&self, fid: Fid, stat: Stat, timeout: Duration) -> Result<()> {
        let remote = self.binding(fid)?.writable_remote()?;
        remote.client.wstat_timeout(remote.fid, stat, timeout)
    }

    pub fn wstat(&self, fid: Fid, stat: Stat) -> Result<()> {
        let remote = self.binding(fid)?.writable_remote()?;
        remote.client.wstat(remote.fid, stat)
    }

    pub fn rename_at_timeout(
        &self,
        olddirfid: Fid,
        oldname: &[u8],
        newdirfid: Fid,
        newname: &[u8],
        timeout: Duration,
    ) -> Result<()> {
        let olddir = self.binding(olddirfid)?.writable_remote()?;
        let newdir = self.binding(newdirfid)?.writable_remote()?;
        if !olddir.client.same_connection(&newdir.client) {
            return Err(Error::new(
                libc::EXDEV,
                "rename endpoints belong to different namespace services",
            ));
        }
        olddir
            .client
            .rename_at_timeout(olddir.fid, oldname, newdir.fid, newname, timeout)
    }

    pub fn rename_at(
        &self,
        olddirfid: Fid,
        oldname: &[u8],
        newdirfid: Fid,
        newname: &[u8],
    ) -> Result<()> {
        let olddir = self.binding(olddirfid)?.writable_remote()?;
        let newdir = self.binding(newdirfid)?.writable_remote()?;
        if !olddir.client.same_connection(&newdir.client) {
            return Err(Error::new(
                libc::EXDEV,
                "rename endpoints belong to different namespace services",
            ));
        }
        olddir
            .client
            .rename_at(olddir.fid, oldname, newdir.fid, newname)
    }

    pub(crate) fn validate_stat(&self, stat: Stat) -> Result<Stat> {
        if !self.variant().supports_symlinks()
            && (stat.qid.is_symlink() || stat.mode & r9p::qid::DMSYMLINK != 0)
        {
            return Err(Error::new(
                libc::EPROTO,
                "server exposed symlink metadata without negotiating 9P2000.R",
            ));
        }
        Ok(stat)
    }

    fn read_binding(
        &self,
        binding: &FidBinding,
        offset: u64,
        count: u32,
        read_remote: impl FnOnce(RemoteFid) -> Result<Vec<u8>>,
    ) -> Result<Vec<u8>> {
        match &binding.target {
            FidTarget::ReferralDirectory => {
                self.read_referral_directory(&binding.namespace_path, offset, count)
            }
            FidTarget::Remote(remote) => read_remote(remote.clone()),
        }
    }
}
