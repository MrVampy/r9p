use crate::{
    codec,
    error::{Error, Result},
    fid::Fid,
    flush::{FlushOutcome, RequestKey},
    message::{RMessage, TMessage},
};
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    net::TcpStream,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    thread,
};

use super::{
    Server, ServerCompletion, ServerConfig, ServerEvent, ServerRequest, ServerRequestKind,
};

pub trait ConnectionStream: Read + Write + Send + 'static {
    fn try_clone_stream(&self) -> io::Result<Self>
    where
        Self: Sized;
}

impl ConnectionStream for TcpStream {
    fn try_clone_stream(&self) -> io::Result<Self> {
        self.try_clone()
    }
}

#[cfg(unix)]
impl ConnectionStream for std::os::unix::net::UnixStream {
    fn try_clone_stream(&self) -> io::Result<Self> {
        self.try_clone()
    }
}

pub trait ConnectionHandler: Send + Sync + 'static {
    fn perform(
        &self,
        request: &ServerRequest,
        cancel: Option<&AtomicBool>,
    ) -> Result<ServerCompletion>;

    fn is_async(&self, _request: &ServerRequest) -> bool {
        false
    }

    fn cancellation_fid(&self, _request: &ServerRequest) -> Option<Fid> {
        None
    }

    fn reset(&self) -> Result<()> {
        Ok(())
    }

    fn wake_after_cancel(&self) {}
}

pub fn serve_connection<S, H>(stream: S, config: ServerConfig, handler: Arc<H>) -> Result<()>
where
    S: ConnectionStream,
    H: ConnectionHandler + ?Sized,
{
    let max_async_requests = config.max_async_requests;
    let mut reader = stream
        .try_clone_stream()
        .map_err(|error| Error::new(format!("clone 9P stream: {error}")))?;
    let writer = Arc::new(Mutex::new(stream));
    let server = Arc::new(Mutex::new(Server::with_config((), config)));
    let pending = Arc::new(Mutex::new(BTreeMap::new()));
    let active_async_workers = Arc::new(AsyncWorkerTracker::default());

    let result = serve_loop(
        &mut reader,
        &writer,
        &server,
        &pending,
        &active_async_workers,
        &handler,
        max_async_requests,
    );
    let cleanup = reset_connection(&server, &pending, &active_async_workers, handler.as_ref());
    match result {
        Err(error) => Err(error),
        Ok(()) => cleanup,
    }
}

fn serve_loop<S, H>(
    reader: &mut S,
    writer: &Arc<Mutex<S>>,
    server: &Arc<Mutex<Server<()>>>,
    pending: &CancelMap,
    active_async_workers: &ActiveAsyncWorkers,
    handler: &Arc<H>,
    max_async_requests: usize,
) -> Result<()>
where
    S: ConnectionStream,
    H: ConnectionHandler + ?Sized,
{
    loop {
        let msize = server
            .lock()
            .map_err(|_| Error::from_static("9P server session poisoned"))?
            .session()
            .msize();
        let message = match codec::read_tmessage_checked(reader, msize) {
            Ok(Some(message)) => message,
            Ok(None) => return Ok(()),
            Err(error) => return Err(error),
        };

        if matches!(message, TMessage::Version { .. }) {
            reset_connection(server, pending, active_async_workers, handler.as_ref())?;
        }

        let (event, cancelled) = admit(server, pending, message)?;
        if cancelled {
            handler.wake_after_cancel();
        }

        match event {
            ServerEvent::Reply(reply) => write_reply(server, writer, &reply)?,
            ServerEvent::Flush { reply, .. } => write_reply(server, writer, &reply)?,
            ServerEvent::Dispatch(request) if handler.is_async(&request) => {
                dispatch_async(
                    Arc::clone(server),
                    Arc::clone(writer),
                    Arc::clone(pending),
                    Arc::clone(active_async_workers),
                    Arc::clone(handler),
                    request,
                    max_async_requests,
                )?;
            }
            ServerEvent::Dispatch(request) => {
                let completion = handler.perform(&request, None);
                complete_and_write(server, writer, request, completion)?;
            }
        }
    }
}

struct PendingCancel {
    fid: Option<Fid>,
    cancel: Arc<AtomicBool>,
}

type CancelMap = Arc<Mutex<BTreeMap<RequestKey, PendingCancel>>>;
type ActiveAsyncWorkers = Arc<AsyncWorkerTracker>;

#[derive(Default)]
struct AsyncWorkerTracker {
    active: Mutex<usize>,
    idle: Condvar,
}

impl AsyncWorkerTracker {
    fn try_acquire(&self, limit: usize) -> Result<bool> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| Error::from_static("9P async worker tracker poisoned"))?;
        if *active >= limit {
            return Ok(false);
        }
        *active += 1;
        Ok(true)
    }

    fn release(&self) {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        debug_assert!(*active > 0, "active async worker count underflow");
        *active = active.saturating_sub(1);
        if *active == 0 {
            self.idle.notify_all();
        }
    }

    fn wait_until_idle(&self) -> Result<()> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| Error::from_static("9P async worker tracker poisoned"))?;
        while *active != 0 {
            active = self
                .idle
                .wait(active)
                .map_err(|_| Error::from_static("9P async worker tracker poisoned"))?;
        }
        Ok(())
    }
}

struct ActiveAsyncWorker {
    key: RequestKey,
    pending: CancelMap,
    active: ActiveAsyncWorkers,
}

impl Drop for ActiveAsyncWorker {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&self.key);
        }
        self.active.release();
    }
}

fn admit(
    server: &Arc<Mutex<Server<()>>>,
    pending: &CancelMap,
    message: TMessage,
) -> Result<(ServerEvent, bool)> {
    let mut server = server
        .lock()
        .map_err(|_| Error::from_static("9P server session poisoned"))?;
    let event = server.admit(message);
    let cancelled = match &event {
        ServerEvent::Flush {
            outcome: FlushOutcome::Cancelled(key),
            ..
        } => cancel_key(pending, *key)?,
        ServerEvent::Dispatch(ServerRequest {
            kind: ServerRequestKind::Clunk { fid, .. },
            ..
        }) => cancel_fid(pending, *fid)?,
        _ => false,
    };
    Ok((event, cancelled))
}

fn dispatch_async<S, H>(
    server: Arc<Mutex<Server<()>>>,
    writer: Arc<Mutex<S>>,
    pending: CancelMap,
    active_async_workers: ActiveAsyncWorkers,
    handler: Arc<H>,
    request: ServerRequest,
    max_async_requests: usize,
) -> Result<()>
where
    S: ConnectionStream,
    H: ConnectionHandler + ?Sized,
{
    let key = request.key;
    let cancel = Arc::new(AtomicBool::new(false));
    let cancellation_fid = handler.cancellation_fid(&request);
    {
        let mut pending = pending
            .lock()
            .map_err(|_| Error::from_static("9P cancellation map poisoned"))?;
        if !active_async_workers.try_acquire(max_async_requests)? {
            drop(pending);
            return complete_and_write(
                &server,
                &writer,
                request,
                Err(Error::from_static("too many asynchronous 9P requests")),
            );
        }
        pending.insert(
            key,
            PendingCancel {
                fid: cancellation_fid,
                cancel: Arc::clone(&cancel),
            },
        );
    }
    let worker_server = Arc::clone(&server);
    let worker_writer = Arc::clone(&writer);
    let worker_pending = Arc::clone(&pending);
    let worker_active = Arc::clone(&active_async_workers);
    let worker_handler = Arc::clone(&handler);
    let worker_request = request.clone();
    let spawn = thread::Builder::new()
        .name(format!("r9p-request-{}", key.tag))
        .spawn(move || {
            let _active_worker = ActiveAsyncWorker {
                key,
                pending: worker_pending,
                active: worker_active,
            };
            let completion = worker_handler.perform(&worker_request, Some(cancel.as_ref()));
            let _ = complete_and_write(&worker_server, &worker_writer, worker_request, completion);
        });
    if let Err(error) = spawn {
        if let Ok(mut pending) = pending.lock() {
            pending.remove(&key);
        }
        active_async_workers.release();
        complete_and_write(
            &server,
            &writer,
            request,
            Err(Error::new(format!("spawn 9P request worker: {error}"))),
        )?;
    }
    Ok(())
}

fn complete_and_write<S>(
    server: &Arc<Mutex<Server<()>>>,
    writer: &Arc<Mutex<S>>,
    request: ServerRequest,
    completion: Result<ServerCompletion>,
) -> Result<()>
where
    S: ConnectionStream,
{
    let mut writer = writer
        .lock()
        .map_err(|_| Error::from_static("9P response writer poisoned"))?;
    let (reply, msize) = {
        let mut server = server
            .lock()
            .map_err(|_| Error::from_static("9P server session poisoned"))?;
        let reply = server.complete(request, completion);
        (reply, server.session().msize())
    };
    if let Some(reply) = reply {
        codec::write_rmessage_checked(&mut *writer, msize, &reply)?;
    }
    Ok(())
}

fn write_reply<S>(
    server: &Arc<Mutex<Server<()>>>,
    writer: &Arc<Mutex<S>>,
    reply: &RMessage,
) -> Result<()>
where
    S: ConnectionStream,
{
    let mut writer = writer
        .lock()
        .map_err(|_| Error::from_static("9P response writer poisoned"))?;
    let msize = server
        .lock()
        .map_err(|_| Error::from_static("9P server session poisoned"))?
        .session()
        .msize();
    codec::write_rmessage_checked(&mut *writer, msize, reply)
}

fn cancel_key(pending: &CancelMap, key: RequestKey) -> Result<bool> {
    let pending = pending
        .lock()
        .map_err(|_| Error::from_static("9P cancellation map poisoned"))?
        .remove(&key);
    if let Some(pending) = pending {
        pending.cancel.store(true, Ordering::SeqCst);
        Ok(true)
    } else {
        Ok(false)
    }
}

fn cancel_fid(pending: &CancelMap, fid: Fid) -> Result<bool> {
    let mut pending = pending
        .lock()
        .map_err(|_| Error::from_static("9P cancellation map poisoned"))?;
    let keys: Vec<RequestKey> = pending
        .iter()
        .filter_map(|(key, cancellation)| (cancellation.fid == Some(fid)).then_some(*key))
        .collect();
    let mut cancelled = false;
    for key in keys {
        if let Some(cancellation) = pending.remove(&key) {
            cancellation.cancel.store(true, Ordering::SeqCst);
            cancelled = true;
        }
    }
    Ok(cancelled)
}

fn cancel_all(pending: &CancelMap) -> Result<bool> {
    let pending = {
        let mut pending = pending
            .lock()
            .map_err(|_| Error::from_static("9P cancellation map poisoned"))?;
        std::mem::take(&mut *pending)
    };
    let cancelled = !pending.is_empty();
    for (_, cancellation) in pending {
        cancellation.cancel.store(true, Ordering::SeqCst);
    }
    Ok(cancelled)
}

fn reset_connection<H>(
    server: &Arc<Mutex<Server<()>>>,
    pending: &CancelMap,
    active_async_workers: &ActiveAsyncWorkers,
    handler: &H,
) -> Result<()>
where
    H: ConnectionHandler + ?Sized,
{
    let cancelled = {
        let mut server = server
            .lock()
            .map_err(|_| Error::from_static("9P server session poisoned"))?;
        server.session_mut().request_table().reset();
        cancel_all(pending)?
    };
    if cancelled {
        handler.wake_after_cancel();
    }
    active_async_workers.wait_until_idle()?;
    handler.reset()
}
