//! High-level FUSE event loop and opcode dispatch.

use super::mount::MountCleanup;
use super::{
    reply::{read_struct, reply_bytes, reply_error},
    wire::{
        FuseInHeader, FuseInitIn, FuseInitOut, FuseInterruptIn, FUSE_ACCESS, FUSE_ASYNC_READ,
        FUSE_ATOMIC_O_TRUNC, FUSE_AUTO_INVAL_DATA, FUSE_BATCH_FORGET, FUSE_BIG_WRITES,
        FUSE_BUFFER_SIZE, FUSE_COMPAT_22_INIT_OUT_SIZE, FUSE_COMPAT_INIT_OUT_SIZE, FUSE_CREATE,
        FUSE_DESTROY, FUSE_DO_READDIRPLUS, FUSE_FLUSH, FUSE_FORGET, FUSE_FSYNC, FUSE_FSYNCDIR,
        FUSE_GETATTR, FUSE_GETLK, FUSE_GETXATTR, FUSE_INIT, FUSE_INTERRUPT,
        FUSE_KERNEL_MINOR_VERSION, FUSE_KERNEL_VERSION, FUSE_LINK, FUSE_LISTXATTR, FUSE_LOOKUP,
        FUSE_MAX_PAGES, FUSE_MKDIR, FUSE_MKNOD, FUSE_OPEN, FUSE_OPENDIR, FUSE_PARALLEL_DIROPS,
        FUSE_POLL, FUSE_READ, FUSE_READDIR, FUSE_READDIRPLUS, FUSE_READDIRPLUS_AUTO, FUSE_READLINK,
        FUSE_RELEASE, FUSE_RELEASEDIR, FUSE_REMOVEXATTR, FUSE_RENAME, FUSE_RMDIR, FUSE_SETATTR,
        FUSE_SETLK, FUSE_SETLKW, FUSE_SETXATTR, FUSE_STATFS, FUSE_SYMLINK, FUSE_UNLINK, FUSE_WRITE,
    },
    R9pFuse,
};
use crate::{
    error::{Error, Result},
    node::{NodeTable, ROOT_NODEID},
};
use session::with_fuse_unique;
use std::{
    fs::File,
    io::{self, Read},
    mem::size_of,
    os::{fd::AsRawFd, unix::net::UnixStream},
    panic::{self, AssertUnwindSafe},
    sync::{
        mpsc::{sync_channel, Receiver, SyncSender},
        Arc, Mutex, MutexGuard,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const SOURCE_RECONNECT_RETRY_DELAY: Duration = Duration::from_millis(25);

impl R9pFuse {
    pub(super) fn run(
        &self,
        file: &mut File,
        feed_events: Option<session::feed::FeedEventReceiver>,
    ) -> Result<()> {
        self.run_loop(file, feed_events, None)
    }

    pub(super) fn run_managed(
        &self,
        file: &mut File,
        shutdown: UnixStream,
        cleanup: MountCleanup,
    ) -> Result<()> {
        self.run_loop(file, None, Some((shutdown, cleanup)))
    }

    fn run_loop(
        &self,
        file: &mut File,
        feed_events: Option<session::feed::FeedEventReceiver>,
        shutdown: Option<(UnixStream, MountCleanup)>,
    ) -> Result<()> {
        let mut workers = WorkerPool::start(self)?;
        let mut change_feed = match feed_events {
            Some(receiver) => self.start_session_feed_events(file, receiver)?,
            None => self.start_change_feed(file)?,
        };
        let mut buf = vec![0_u8; FUSE_BUFFER_SIZE];
        let mut initialized = false;
        loop {
            if let Some((shutdown, cleanup)) = shutdown.as_ref() {
                match wait_for_managed_input(file.as_raw_fd(), shutdown.as_raw_fd())
                    .map_err(|error| Error::io("wait for managed FUSE input", error))?
                {
                    ManagedInput::Fuse => {}
                    ManagedInput::Shutdown => {
                        cleanup.cleanup();
                        if let Some(feed) = change_feed.take() {
                            feed.stop_and_join();
                        }
                        workers.shutdown();
                        return Ok(());
                    }
                }
            }
            let n = match file.read(&mut buf) {
                Ok(0) => {
                    if let Some(feed) = change_feed.take() {
                        feed.stop_and_join();
                    }
                    if !initialized {
                        workers.shutdown();
                        return Err(Error::new(
                            libc::ENODEV,
                            "FUSE session ended before initialization",
                        ));
                    }
                    return Ok(());
                }
                Ok(n) => n,
                Err(error) if error.raw_os_error() == Some(libc::ENODEV) => {
                    if let Some(feed) = change_feed.take() {
                        feed.stop_and_join();
                    }
                    if !initialized {
                        workers.shutdown();
                        return Err(Error::new(
                            libc::ENODEV,
                            "FUSE session ended before initialization",
                        ));
                    }
                    return Ok(());
                }
                Err(error) => return Err(Error::io("read /dev/fuse", error)),
            };
            if n < size_of::<FuseInHeader>() {
                return Err(Error::new(libc::EPROTO, "short FUSE request"));
            }
            let header = read_struct::<FuseInHeader>(&buf[..size_of::<FuseInHeader>()])?;
            if usize::try_from(header.len).unwrap_or(usize::MAX) != n {
                return Err(Error::new(libc::EPROTO, "FUSE request length mismatch"));
            }
            let payload = &buf[size_of::<FuseInHeader>()..n];
            if self.config.debug {
                eprintln!(
                    "r9p mount: opcode={} unique={} nodeid={}",
                    header.opcode, header.unique, header.nodeid
                );
            }
            if header.opcode == FUSE_INIT {
                initialized = true;
            }
            if header.opcode == FUSE_DESTROY {
                let mut writer = file
                    .try_clone()
                    .map_err(|error| Error::io("clone /dev/fuse writer", error))?;
                let mut worker = self.clone();
                worker.dispatch(&mut writer, header, payload)?;
                if let Some(feed) = change_feed.take() {
                    feed.stop_and_join();
                }
                workers.shutdown();
                if !initialized {
                    return Err(Error::new(
                        libc::ENODEV,
                        "FUSE session destroyed before initialization",
                    ));
                }
                return Ok(());
            }
            let writer = file
                .try_clone()
                .map_err(|error| Error::io("clone /dev/fuse writer", error))?;
            let payload = payload.to_vec();
            workers.submit(FuseJob {
                writer,
                header,
                payload,
            })?;
        }
    }

    pub(super) fn nodes(&self) -> Result<MutexGuard<'_, NodeTable>> {
        self.nodes
            .lock()
            .map_err(|_| Error::new(libc::EIO, "node table lock poisoned"))
    }

    fn dispatch(&mut self, file: &mut File, header: FuseInHeader, payload: &[u8]) -> Result<()> {
        let result = match header.opcode {
            FUSE_INIT => self.fuse_init(file, header, payload),
            FUSE_LOOKUP => self.lookup(file, header, payload),
            FUSE_FORGET => self.forget(header, payload),
            FUSE_BATCH_FORGET => self.batch_forget(payload),
            FUSE_GETATTR => self.getattr(file, header),
            FUSE_SETATTR => self.setattr(file, header, payload),
            FUSE_READLINK => self.readlink(file, header),
            FUSE_OPEN => self.open(file, header, payload, false),
            FUSE_OPENDIR => self.open(file, header, payload, true),
            FUSE_READ => self.read(file, header, payload),
            FUSE_WRITE => self.write(file, header, payload),
            FUSE_RELEASE | FUSE_RELEASEDIR => self.release(file, header, payload),
            FUSE_READDIR => self.readdir(file, header, payload),
            FUSE_READDIRPLUS => self.readdirplus(file, header, payload),
            FUSE_CREATE => self.create(file, header, payload),
            FUSE_MKDIR => self.mkdir(file, header, payload),
            FUSE_MKNOD => self.mknod(file, header, payload),
            FUSE_UNLINK => self.remove(file, header, payload, false),
            FUSE_RMDIR => self.remove(file, header, payload, true),
            FUSE_RENAME => self.rename(file, header, payload),
            FUSE_SYMLINK | FUSE_LINK | FUSE_SETXATTR | FUSE_GETXATTR | FUSE_LISTXATTR
            | FUSE_REMOVEXATTR => reply_error(file, header.unique, libc::ENOTSUP),
            FUSE_GETLK => self.getlk(file, header, payload),
            FUSE_SETLK | FUSE_SETLKW => self.setlk(file, header),
            FUSE_FLUSH => self.flush(file, header, payload),
            FUSE_FSYNC => self.fsync(file, header, payload),
            FUSE_FSYNCDIR => self.fsyncdir(file, header, payload),
            FUSE_ACCESS => self.access(file, header, payload),
            FUSE_INTERRUPT => self.interrupt(header, payload),
            FUSE_STATFS => self.statfs(file, header),
            FUSE_DESTROY => Ok(()),
            FUSE_POLL => self.poll(file, header, payload),
            _ => reply_error(file, header.unique, libc::ENOSYS),
        };
        if let Err(error) = result {
            let context = self.diagnostic_context(header, payload);
            self.record_diagnostic_with_context(
                "operation_error",
                header,
                error.errno,
                error.message().to_string(),
                context,
            );
            if self.config.debug || should_log_operation_error(&error) {
                eprintln!(
                    "r9p mount: opcode={} unique={} error={} {}",
                    header.opcode,
                    header.unique,
                    error.errno,
                    error.message()
                );
            }
            reply_error(file, header.unique, error.errno)?;
        }
        Ok(())
    }

    fn interrupt(&mut self, header: FuseInHeader, payload: &[u8]) -> Result<()> {
        let Ok(input) = read_struct::<FuseInterruptIn>(payload) else {
            return Ok(());
        };
        let flushed = self
            .client
            .snapshot()
            .and_then(|client| client.interrupt_fuse_unique(input.unique, self.interrupt_timeout()))
            .unwrap_or(0);
        if self.config.debug {
            eprintln!(
                "r9p mount: interrupt unique={} target={} flushed={}",
                header.unique, input.unique, flushed
            );
        }
        Ok(())
    }

    fn fuse_init(&mut self, file: &mut File, header: FuseInHeader, payload: &[u8]) -> Result<()> {
        let input = read_struct::<FuseInitIn>(payload)?;
        let page_size = system_page_size()?;
        // Capabilities we both want and the kernel advertised. Each opt-in is
        // safe with our current handlers: ATOMIC_O_TRUNC short-circuits the
        // separate truncate round trip on OPEN, BIG_WRITES is governed by
        // max_write, AUTO_INVAL_DATA invalidates page-cache pages when mtime
        // changes (relevant once non-zero attr_timeout returns),
        // MAX_PAGES admits the same maximum byte count for reads and writes,
        // PARALLEL_DIROPS unblocks concurrent lookups inside one dir, and
        // adaptive READDIRPLUS lets the kernel seed dentry/attribute cache
        // when traversal tools actually inspect returned entries. We do not
        // advertise EXPORT_SUPPORT: forgotten FUSE nodeids are intentionally
        // retired, so the bridge cannot satisfy exportfs stale-handle lookup.
        // Nor do we advertise DONT_MASK: Linux must apply the caller's umask
        // before r9p forwards the requested permission bits to the 9P server.
        let output = fuse_init_out(
            input,
            self.config.max_background,
            self.config.congestion_threshold,
            page_size,
            self.max_request_bytes,
        )?;
        self.record_diagnostic(
            "fuse_initialized",
            header,
            0,
            format!(
                "max_request_bytes={} page_size={} max_pages={} max_pages_active={}",
                output.max_write,
                page_size,
                output.max_pages,
                output.flags & FUSE_MAX_PAGES != 0
            ),
        );
        let size = init_out_size(output.minor);
        reply_bytes(file, header.unique, &init_out_bytes(&output)[..size])
    }

    pub(super) fn reconnect(&mut self) -> Result<()> {
        let reconnect = Arc::clone(&self.reconnect);
        let _reconnect_guard = reconnect
            .lock()
            .map_err(|_| Error::new(libc::EIO, "reconnect lock poisoned"))?;
        self.status.set_data_session("reconnecting", None);
        if self.config.debug {
            eprintln!(
                "r9p mount: reconnecting from {} across {} candidate(s)",
                self.client.active_address(),
                self.client.candidate_addresses().len()
            );
        }
        let stale = {
            let mut nodes = self.nodes()?;
            let nodeids = nodes
                .rebind_paths()
                .into_iter()
                .map(|(nodeid, _)| nodeid)
                .collect::<Vec<_>>();
            let stale_count = nodeids.len();
            let _ = nodes.apply_rebind_results(Vec::new(), nodeids);
            stale_count
        };
        let client = match self.client.reconnect() {
            Ok(client) => client,
            Err(error) => {
                self.status
                    .set_data_session("degraded", Some(error.message().to_string()));
                return Err(error.into());
            }
        };
        self.status.set_transport(self.client.active_address());
        let (root_fid, root_stat) = match self.reconnect_source_binding(&client) {
            Ok(binding) => binding,
            Err(error) => {
                self.status
                    .set_data_session("degraded", Some(error.message().to_string()));
                return Err(error);
            }
        };
        let lazy_rebind_count = {
            let mut nodes = self.nodes()?;
            let _ =
                nodes.apply_rebind_results(vec![(ROOT_NODEID, root_fid, root_stat)], Vec::new());
            let lazy_rebind_count = stale.saturating_sub(1);
            lazy_rebind_count
        };
        self.status.set_data_session("connected", None);
        self.record_mount_diagnostic(
            "transport_reconnected",
            0,
            format!(
                "endpoint={} lazy_rebind_count={lazy_rebind_count}",
                self.client.active_address()
            ),
        );
        if self.config.debug {
            eprintln!(
                "r9p mount: reconnect marked {} node bindings for lazy rebind",
                lazy_rebind_count
            );
            eprintln!(
                "r9p mount: reconnect complete on {}",
                self.client.active_address()
            );
        }
        Ok(())
    }

    fn reconnect_source_binding(&self, client: &session::Client) -> Result<(r9p::Fid, r9p::Stat)> {
        let started = Instant::now();
        loop {
            let elapsed = started.elapsed();
            let remaining = self.config.lookup_timeout.saturating_sub(elapsed);
            if remaining.is_zero() {
                return Err(Error::new(
                    libc::ETIMEDOUT,
                    "mounted namespace source did not return before reconnect deadline",
                ));
            }
            match self.source_binding(client, remaining) {
                Ok(binding) => return Ok(binding),
                Err(error)
                    if source_binding_retryable(&error)
                        && started.elapsed() < self.config.lookup_timeout =>
                {
                    thread::sleep(
                        SOURCE_RECONNECT_RETRY_DELAY
                            .min(self.config.lookup_timeout.saturating_sub(started.elapsed())),
                    );
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub(super) fn recover_namespace_shape(&mut self, nodeid: u64) -> Result<()> {
        match self.refresh_node(nodeid) {
            Ok(()) => {
                let force = self
                    .shape_recovery
                    .lock()
                    .map(|mut recovery| recovery.note())
                    .unwrap_or(false);
                if force {
                    if self.config.debug {
                        eprintln!(
                            "r9p mount: repeated namespace shape failures; forcing reconnect"
                        );
                    }
                    self.reconnect()?;
                }
                Ok(())
            }
            Err(refresh_error) => {
                if self.config.debug {
                    eprintln!(
                        "r9p mount: node refresh failed ({}); escalating to reconnect",
                        refresh_error.message()
                    );
                }
                self.reconnect().map_err(|_| refresh_error)
            }
        }
    }

    pub(super) fn refresh_node(&mut self, nodeid: u64) -> Result<()> {
        if self.config.debug {
            eprintln!("r9p mount: refreshing path-backed node {nodeid}");
        }
        if nodeid == ROOT_NODEID {
            let client = self.client.snapshot()?;
            let (root_fid, stat) = self.source_binding(&client, self.config.lookup_timeout)?;
            let old_fid = self.nodes()?.replace_binding(nodeid, root_fid, stat)?;
            if let Some(old_fid) = old_fid {
                let _ = client.clunk_timeout(old_fid, self.config.control_timeout);
            }
            return Ok(());
        }
        let path = {
            let nodes = self.nodes()?;
            nodes.node(nodeid)?.path.clone()
        };
        let client = self.client.snapshot()?;
        let fid = self.walk_from_source(&client, &path, self.config.lookup_timeout)?;
        let stat = client.stat_timeout(fid, self.config.lookup_timeout)?;
        let old_fid = self.nodes()?.replace_binding(nodeid, fid, stat)?;
        if let Some(old_fid) = old_fid {
            let _ = client.clunk_timeout(old_fid, self.config.control_timeout);
        }
        Ok(())
    }
}

fn source_binding_retryable(error: &Error) -> bool {
    matches!(
        error.errno,
        libc::ENOENT
            | libc::ESTALE
            | libc::EAGAIN
            | libc::ETIMEDOUT
            | libc::ENOTCONN
            | libc::ECONNREFUSED
            | libc::ECONNRESET
            | libc::ECONNABORTED
            | libc::EPIPE
            | libc::ENETDOWN
            | libc::ENETUNREACH
            | libc::EHOSTDOWN
            | libc::EHOSTUNREACH
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ManagedInput {
    Fuse,
    Shutdown,
}

pub(super) fn wait_for_managed_input(
    fuse_fd: libc::c_int,
    shutdown_fd: libc::c_int,
) -> io::Result<ManagedInput> {
    loop {
        let mut descriptors = [
            libc::pollfd {
                fd: fuse_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: shutdown_fd,
                events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
                revents: 0,
            },
        ];
        let ready = unsafe {
            libc::poll(
                descriptors.as_mut_ptr(),
                descriptors.len() as libc::nfds_t,
                -1,
            )
        };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if descriptors[1].revents != 0 {
            return Ok(ManagedInput::Shutdown);
        }
        if descriptors[0].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP | libc::POLLNVAL)
            != 0
        {
            return Ok(ManagedInput::Fuse);
        }
    }
}

struct FuseJob {
    writer: File,
    header: FuseInHeader,
    payload: Vec<u8>,
}

struct WorkerPool {
    sender: Option<SyncSender<FuseJob>>,
    handles: Vec<JoinHandle<()>>,
}

impl WorkerPool {
    fn start(fs: &R9pFuse) -> Result<Self> {
        let worker_count = fs.config.max_workers.max(1);
        let queue_depth = usize::from(fs.config.max_background).max(1);
        let (sender, receiver) = sync_channel(queue_depth);
        let receiver = Arc::new(Mutex::new(receiver));
        let mut handles = Vec::with_capacity(worker_count);
        for worker_index in 0..worker_count {
            let receiver = Arc::clone(&receiver);
            let worker = fs.clone();
            let handle = thread::Builder::new()
                .name(format!("r9p-fuse-{worker_index}"))
                .spawn(move || fuse_worker_loop(worker, receiver))
                .map_err(|error| Error::io("spawn FUSE worker", error))?;
            handles.push(handle);
        }
        Ok(Self {
            sender: Some(sender),
            handles,
        })
    }

    fn submit(&self, job: FuseJob) -> Result<()> {
        let sender = self
            .sender
            .as_ref()
            .ok_or_else(|| Error::new(libc::EIO, "FUSE worker queue is shut down"))?;
        sender
            .send(job)
            .map_err(|_| Error::new(libc::EIO, "FUSE worker queue is closed"))
    }

    fn shutdown(&mut self) {
        self.sender.take();
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn fuse_worker_loop(mut fs: R9pFuse, receiver: Arc<Mutex<Receiver<FuseJob>>>) {
    loop {
        let job = {
            let receiver = match receiver.lock() {
                Ok(receiver) => receiver,
                Err(_) => return,
            };
            match receiver.recv() {
                Ok(job) => job,
                Err(_) => return,
            }
        };
        dispatch_fuse_job(&mut fs, job);
    }
}

fn dispatch_fuse_job(fs: &mut R9pFuse, mut job: FuseJob) {
    let header = job.header;
    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        with_fuse_unique(header.unique, || {
            fs.dispatch(&mut job.writer, header, &job.payload)
        })
    }));
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            fs.record_diagnostic(
                "dispatch_failure",
                header,
                error.errno,
                error.message().to_string(),
            );
            eprintln!(
                "r9p mount: opcode={} unique={} dispatch failure={} {}",
                header.opcode,
                header.unique,
                error.errno,
                error.message()
            );
            let _ = reply_error(&mut job.writer, header.unique, error.errno);
        }
        Err(_) => {
            fs.record_diagnostic("worker_panic", header, libc::EIO, "FUSE worker panic");
            eprintln!(
                "r9p mount: opcode={} unique={} worker panic",
                header.opcode, header.unique
            );
            let _ = reply_error(&mut job.writer, header.unique, libc::EIO);
        }
    }
}

fn should_log_operation_error(error: &Error) -> bool {
    matches!(
        error.errno,
        libc::EIO
            | libc::EREMOTEIO
            | libc::EPROTO
            | libc::ETIMEDOUT
            | libc::ENOTCONN
            | libc::ECONNRESET
            | libc::ECONNABORTED
    )
}

pub(super) fn supported_init_flags() -> u32 {
    FUSE_ASYNC_READ
        | FUSE_ATOMIC_O_TRUNC
        | FUSE_BIG_WRITES
        | FUSE_AUTO_INVAL_DATA
        | FUSE_DO_READDIRPLUS
        | FUSE_READDIRPLUS_AUTO
        | FUSE_PARALLEL_DIROPS
        | FUSE_MAX_PAGES
}

pub(super) fn fuse_init_out(
    input: FuseInitIn,
    max_background: u16,
    congestion_threshold: u16,
    page_size: u32,
    max_request_bytes: u32,
) -> Result<FuseInitOut> {
    let max_pages = max_request_pages(max_request_bytes, page_size)?;
    let max_write = u32::from(max_pages)
        .checked_mul(page_size)
        .ok_or_else(|| Error::new(libc::EOVERFLOW, "FUSE request byte count overflow"))?
        .min(max_request_bytes);
    let mut output = FuseInitOut {
        major: FUSE_KERNEL_VERSION,
        minor: input.minor.min(FUSE_KERNEL_MINOR_VERSION),
        max_readahead: input.max_readahead,
        flags: input.flags & supported_init_flags(),
        max_background,
        congestion_threshold,
        max_write,
        time_gran: 1,
        max_pages,
        map_alignment: 0,
        unused: [0; 8],
    };
    output.flags |= FUSE_BIG_WRITES;
    Ok(output)
}

pub(super) fn max_request_pages(max_request_bytes: u32, page_size: u32) -> Result<u16> {
    if max_request_bytes == 0 || page_size == 0 {
        return Err(Error::new(
            libc::EINVAL,
            "FUSE request bytes and page size must be nonzero",
        ));
    }
    u16::try_from((max_request_bytes / page_size).max(1))
        .map_err(|_| Error::new(libc::EOVERFLOW, "FUSE request page count overflow"))
}

fn system_page_size() -> Result<u32> {
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page_size <= 0 {
        return Err(Error::new(libc::EIO, "could not read the system page size"));
    }
    u32::try_from(page_size).map_err(|_| Error::new(libc::EOVERFLOW, "system page size overflow"))
}

fn init_out_size(minor: u32) -> usize {
    if minor < 5 {
        FUSE_COMPAT_INIT_OUT_SIZE
    } else if minor < 23 {
        FUSE_COMPAT_22_INIT_OUT_SIZE
    } else {
        size_of::<FuseInitOut>()
    }
}

fn init_out_bytes(value: &FuseInitOut) -> &[u8] {
    unsafe {
        std::slice::from_raw_parts(
            (value as *const FuseInitOut).cast::<u8>(),
            size_of::<FuseInitOut>(),
        )
    }
}
