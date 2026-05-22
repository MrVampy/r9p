//! High-level FUSE event loop and opcode dispatch.

use super::{
    reply::{read_struct, reply_bytes, reply_error},
    wire::{
        FuseInHeader, FuseInitIn, FuseInitOut, FuseInterruptIn, DEFAULT_MAX_WRITE, FUSE_ACCESS,
        FUSE_ASYNC_READ, FUSE_ATOMIC_O_TRUNC, FUSE_AUTO_INVAL_DATA, FUSE_BATCH_FORGET,
        FUSE_BIG_WRITES, FUSE_BUFFER_SIZE, FUSE_COMPAT_22_INIT_OUT_SIZE, FUSE_COMPAT_INIT_OUT_SIZE,
        FUSE_CREATE, FUSE_DESTROY, FUSE_DONT_MASK, FUSE_DO_READDIRPLUS, FUSE_EXPORT_SUPPORT,
        FUSE_FLUSH, FUSE_FORGET, FUSE_FSYNC, FUSE_FSYNCDIR, FUSE_GETATTR, FUSE_GETLK,
        FUSE_GETXATTR, FUSE_INIT, FUSE_INTERRUPT, FUSE_KERNEL_MINOR_VERSION, FUSE_KERNEL_VERSION,
        FUSE_LINK, FUSE_LISTXATTR, FUSE_LOOKUP, FUSE_MKDIR, FUSE_MKNOD, FUSE_OPEN, FUSE_OPENDIR,
        FUSE_PARALLEL_DIROPS, FUSE_POLL, FUSE_READ, FUSE_READDIR, FUSE_READDIRPLUS, FUSE_RELEASE,
        FUSE_RELEASEDIR, FUSE_REMOVEXATTR, FUSE_RENAME, FUSE_RMDIR, FUSE_SETATTR, FUSE_SETLK,
        FUSE_SETLKW, FUSE_SETXATTR, FUSE_STATFS, FUSE_SYMLINK, FUSE_UNLINK, FUSE_WRITE,
    },
    R9pFuse,
};
use crate::{
    error::{Error, Result},
    node::{NodeTable, ROOT_NODEID},
    p9::{with_fuse_unique, Client},
};
use std::{
    fs::File,
    io::Read,
    mem::size_of,
    panic::{self, AssertUnwindSafe},
    sync::{
        mpsc::{sync_channel, Receiver, SyncSender},
        Arc, Mutex, MutexGuard,
    },
    thread::{self, JoinHandle},
};

impl R9pFuse {
    pub(super) fn run(&self, file: &mut File) -> Result<()> {
        let mut workers = WorkerPool::start(self)?;
        let mut change_feed = self.start_change_feed(file)?;
        let mut buf = vec![0_u8; FUSE_BUFFER_SIZE];
        loop {
            let n = match file.read(&mut buf) {
                Ok(0) => {
                    if let Some(feed) = change_feed.take() {
                        feed.stop_and_join();
                    }
                    return Ok(());
                }
                Ok(n) => n,
                Err(error) if error.raw_os_error() == Some(libc::ENODEV) => {
                    if let Some(feed) = change_feed.take() {
                        feed.stop_and_join();
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
        let negotiated_minor = input.minor.min(FUSE_KERNEL_MINOR_VERSION);
        // Capabilities we both want and the kernel advertised. Each opt-in is
        // safe with our current handlers: ATOMIC_O_TRUNC short-circuits the
        // separate truncate round trip on OPEN, EXPORT_SUPPORT allows NFS
        // export and stable inodes, DONT_MASK tells the kernel not to apply
        // umask to mode bits we already pass through verbatim, BIG_WRITES is
        // governed by max_write, AUTO_INVAL_DATA invalidates page-cache pages
        // when mtime changes (relevant once non-zero attr_timeout returns),
        // DO_READDIRPLUS makes Linux ask for directory stats in the same
        // request, and PARALLEL_DIROPS unblocks concurrent lookups inside
        // one dir. We intentionally do not request READDIRPLUS_AUTO: 9P
        // directory reads already return stat data, so adaptive fallback only
        // makes `ls -l` issue avoidable LOOKUP/GETATTR traffic.
        let supported = FUSE_ASYNC_READ
            | FUSE_ATOMIC_O_TRUNC
            | FUSE_EXPORT_SUPPORT
            | FUSE_BIG_WRITES
            | FUSE_DONT_MASK
            | FUSE_AUTO_INVAL_DATA
            | FUSE_DO_READDIRPLUS
            | FUSE_PARALLEL_DIROPS;
        let mut output = FuseInitOut {
            major: FUSE_KERNEL_VERSION,
            minor: negotiated_minor,
            max_readahead: input.max_readahead,
            flags: input.flags & supported,
            max_background: self.config.max_background,
            congestion_threshold: self.config.congestion_threshold,
            max_write: DEFAULT_MAX_WRITE,
            time_gran: 1,
            max_pages: 0,
            map_alignment: 0,
            unused: [0; 8],
        };
        output.flags |= FUSE_BIG_WRITES;
        let size = init_out_size(negotiated_minor);
        reply_bytes(file, header.unique, &init_out_bytes(&output)[..size])
    }

    pub(super) fn reconnect(&mut self) -> Result<()> {
        if self.config.debug {
            eprintln!("r9p mount: reconnecting to {}", self.config.address);
        }
        let tracker = self.client.snapshot()?.tracker();
        let mut client = Client::connect_with_tracker(
            &self.config.address,
            &self.config.uname,
            &self.config.aname,
            self.config.msize,
            tracker,
        )?;
        {
            let mut nodes = self.nodes()?;
            let _ = nodes.rebind_all(&mut client, self.config.lookup_timeout)?;
            self.client.replace(client)?;
        }
        if self.config.debug {
            eprintln!("r9p mount: reconnect complete");
        }
        Ok(())
    }

    pub(super) fn refresh_node(&mut self, nodeid: u64) -> Result<()> {
        if self.config.debug {
            eprintln!("r9p mount: refreshing path-backed node {nodeid}");
        }
        if nodeid == ROOT_NODEID {
            let client = self.client.snapshot()?;
            let root_fid = client.root_fid();
            let stat = client.stat_timeout(root_fid, self.config.lookup_timeout)?;
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
        let fid = client.walk_timeout(client.root_fid(), &path, self.config.lookup_timeout)?;
        let stat = client.stat_timeout(fid, self.config.lookup_timeout)?;
        let old_fid = self.nodes()?.replace_binding(nodeid, fid, stat)?;
        if let Some(old_fid) = old_fid {
            let _ = client.clunk_timeout(old_fid, self.config.control_timeout);
        }
        Ok(())
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
