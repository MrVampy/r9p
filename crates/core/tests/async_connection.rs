#![cfg(unix)]

use r9p::{
    codec,
    error::{Error, Result},
    fid::{Fid, NOFID},
    message::{RMessage, TMessage, NOTAG},
    qid::{Qid, DMDIR},
    server::{
        serve_connection, ConnectionHandler, ServerCompletion, ServerConfig, ServerRequest,
        ServerRequestKind,
    },
    stat::Stat,
};
use std::{
    collections::BTreeSet,
    os::unix::net::UnixStream,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Condvar, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

type TestResult<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Default)]
struct ReadState {
    started: BTreeSet<u64>,
    cancelled: BTreeSet<u64>,
    released: BTreeSet<u64>,
    finished: BTreeSet<u64>,
    active: usize,
    peak_active: usize,
}

struct TestHandler {
    next_attach: AtomicU64,
    resets: AtomicUsize,
    wakes: AtomicUsize,
    reads: Mutex<ReadState>,
    changed: Condvar,
}

impl TestHandler {
    fn new() -> Self {
        Self {
            next_attach: AtomicU64::new(0),
            resets: AtomicUsize::new(0),
            wakes: AtomicUsize::new(0),
            reads: Mutex::new(ReadState::default()),
            changed: Condvar::new(),
        }
    }

    fn wait_until(&self, predicate: impl Fn(&ReadState) -> bool) -> TestResult<()> {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut reads = self.reads.lock().map_err(|_| "test read state poisoned")?;
        while !predicate(&reads) {
            let now = Instant::now();
            if now >= deadline {
                return Err("timed out waiting for test handler state".into());
            }
            let (next, timeout) = self
                .changed
                .wait_timeout(reads, deadline.saturating_duration_since(now))
                .map_err(|_| "test read state poisoned")?;
            reads = next;
            if timeout.timed_out() && !predicate(&reads) {
                return Err("timed out waiting for test handler state".into());
            }
        }
        Ok(())
    }

    fn wait_started(&self, path: u64) -> TestResult<()> {
        self.wait_until(|reads| reads.started.contains(&path))
    }

    fn wait_cancelled(&self, path: u64) -> TestResult<()> {
        self.wait_until(|reads| reads.cancelled.contains(&path))
    }

    fn wait_finished(&self, path: u64) -> TestResult<()> {
        self.wait_until(|reads| reads.finished.contains(&path))
    }

    fn wait_active_at_least(&self, count: usize) -> TestResult<()> {
        self.wait_until(|reads| reads.active >= count)
    }

    fn wait_active(&self, count: usize) -> TestResult<()> {
        self.wait_until(|reads| reads.active == count)
    }

    fn peak_active(&self) -> TestResult<usize> {
        let reads = self.reads.lock().map_err(|_| "test read state poisoned")?;
        Ok(reads.peak_active)
    }

    fn release(&self, path: u64) -> TestResult<()> {
        let mut reads = self.reads.lock().map_err(|_| "test read state poisoned")?;
        reads.released.insert(path);
        self.changed.notify_all();
        Ok(())
    }

    fn was_cancelled(&self, path: u64) -> TestResult<bool> {
        let reads = self.reads.lock().map_err(|_| "test read state poisoned")?;
        Ok(reads.cancelled.contains(&path))
    }
}

impl ConnectionHandler for TestHandler {
    fn perform(
        &self,
        request: &ServerRequest,
        cancel: Option<&AtomicBool>,
    ) -> Result<ServerCompletion> {
        match &request.kind {
            ServerRequestKind::Attach { .. } => {
                let path = self.next_attach.fetch_add(1, Ordering::SeqCst) + 1;
                Ok(ServerCompletion::Attach {
                    qid: Qid::dir(path),
                })
            }
            ServerRequestKind::Read { qid, .. } => {
                let mut reads = self
                    .reads
                    .lock()
                    .map_err(|_| Error::from_static("test read state poisoned"))?;
                reads.active += 1;
                reads.peak_active = reads.peak_active.max(reads.active);
                reads.started.insert(qid.path);
                self.changed.notify_all();
                let cancelled = loop {
                    let cancelled = cancel
                        .map(|cancel| cancel.load(Ordering::SeqCst))
                        .unwrap_or(false);
                    if cancelled {
                        reads.cancelled.insert(qid.path);
                        self.changed.notify_all();
                    }
                    if reads.released.contains(&qid.path) {
                        reads.active -= 1;
                        reads.finished.insert(qid.path);
                        self.changed.notify_all();
                        break cancelled;
                    }
                    reads = self
                        .changed
                        .wait(reads)
                        .map_err(|_| Error::from_static("test read state poisoned"))?;
                };
                if cancelled {
                    Err(Error::from_static("cancelled"))
                } else {
                    Ok(ServerCompletion::Read(r9p::server::ReadData::Bytes(
                        format!("read-{}", qid.path).into_bytes(),
                    )))
                }
            }
            ServerRequestKind::Clunk { .. } => Ok(ServerCompletion::Clunk),
            ServerRequestKind::Stat { qid, .. } => Ok(ServerCompletion::Stat {
                stat: Stat::new(".", *qid, DMDIR | 0o500),
            }),
            _ => Err(Error::from_static("unsupported test request")),
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
        self.resets.fetch_add(1, Ordering::SeqCst);
        let _reads = self
            .reads
            .lock()
            .map_err(|_| Error::from_static("test read state poisoned"))?;
        self.changed.notify_all();
        Ok(())
    }

    fn wake_after_cancel(&self) {
        self.wakes.fetch_add(1, Ordering::SeqCst);
        if let Ok(_reads) = self.reads.lock() {
            self.changed.notify_all();
        }
    }
}

fn start_server(handler: Arc<TestHandler>) -> TestResult<(UnixStream, JoinHandle<Result<()>>)> {
    start_server_with_config(handler, ServerConfig::default())
}

fn start_server_with_config(
    handler: Arc<TestHandler>,
    overrides: ServerConfig,
) -> TestResult<(UnixStream, JoinHandle<Result<()>>)> {
    let (client, server) = UnixStream::pair()?;
    let join = thread::spawn(move || {
        serve_connection(
            server,
            ServerConfig {
                default_msize: 1024,
                max_msize: 1024,
                ..overrides
            },
            handler,
        )
    });
    Ok((client, join))
}

fn send(stream: &mut UnixStream, message: TMessage) -> TestResult<()> {
    codec::write_tmessage(stream, &message)?;
    Ok(())
}

fn receive(stream: &mut UnixStream) -> TestResult<RMessage> {
    codec::read_rmessage(stream)?.ok_or_else(|| "server closed without a reply".into())
}

fn negotiate(stream: &mut UnixStream) -> TestResult<()> {
    send(
        stream,
        TMessage::Version {
            tag: NOTAG,
            msize: 1024,
            version: b"9P2000".to_vec(),
        },
    )?;
    assert_eq!(
        receive(stream)?,
        RMessage::Version {
            tag: NOTAG,
            msize: 1024,
            version: b"9P2000".to_vec(),
        }
    );
    Ok(())
}

fn attach(stream: &mut UnixStream, tag: u16, fid: Fid) -> TestResult<Qid> {
    send(
        stream,
        TMessage::Attach {
            tag,
            fid,
            afid: NOFID,
            uname: b"test".to_vec(),
            aname: Vec::new(),
        },
    )?;
    match receive(stream)? {
        RMessage::Attach {
            tag: reply_tag,
            qid,
        } if reply_tag == tag => Ok(qid),
        reply => Err(format!("unexpected attach reply: {reply:?}").into()),
    }
}

fn finish_server(stream: UnixStream, join: JoinHandle<Result<()>>) -> TestResult<()> {
    drop(stream);
    join.join().map_err(|_| "connection server panicked")??;
    Ok(())
}

#[test]
fn flush_cancels_async_work_and_drops_its_stale_generation() -> TestResult<()> {
    let handler = Arc::new(TestHandler::new());
    let (mut stream, join) = start_server(Arc::clone(&handler))?;
    negotiate(&mut stream)?;
    let qid = attach(&mut stream, 1, 7)?;

    send(
        &mut stream,
        TMessage::Read {
            tag: 2,
            fid: 7,
            offset: 0,
            count: 32,
        },
    )?;
    handler.wait_started(qid.path)?;
    send(&mut stream, TMessage::Flush { tag: 3, oldtag: 2 })?;
    assert_eq!(receive(&mut stream)?, RMessage::Flush { tag: 3 });
    handler.wait_cancelled(qid.path)?;

    send(&mut stream, TMessage::Stat { tag: 2, fid: 7 })?;
    assert!(matches!(
        receive(&mut stream)?,
        RMessage::Stat { tag: 2, .. }
    ));
    handler.release(qid.path)?;
    handler.wait_finished(qid.path)?;
    send(&mut stream, TMessage::Stat { tag: 4, fid: 7 })?;
    assert!(matches!(
        receive(&mut stream)?,
        RMessage::Stat { tag: 4, .. }
    ));
    assert!(handler.wakes.load(Ordering::SeqCst) >= 1);

    finish_server(stream, join)
}

#[test]
fn clunk_cancellation_does_not_leak_across_fid_reuse() -> TestResult<()> {
    let handler = Arc::new(TestHandler::new());
    let (mut stream, join) = start_server(Arc::clone(&handler))?;
    negotiate(&mut stream)?;
    let old = attach(&mut stream, 1, 11)?;

    send(
        &mut stream,
        TMessage::Read {
            tag: 2,
            fid: 11,
            offset: 0,
            count: 32,
        },
    )?;
    handler.wait_started(old.path)?;
    send(&mut stream, TMessage::Clunk { tag: 3, fid: 11 })?;
    assert_eq!(receive(&mut stream)?, RMessage::Clunk { tag: 3 });
    handler.wait_cancelled(old.path)?;

    let new = attach(&mut stream, 4, 11)?;
    send(
        &mut stream,
        TMessage::Read {
            tag: 5,
            fid: 11,
            offset: 0,
            count: 32,
        },
    )?;
    handler.wait_started(new.path)?;
    assert!(!handler.was_cancelled(new.path)?);
    handler.release(new.path)?;
    assert_eq!(
        receive(&mut stream)?,
        RMessage::Read {
            tag: 5,
            data: format!("read-{}", new.path).into_bytes(),
        }
    );

    handler.release(old.path)?;
    assert_eq!(
        receive(&mut stream)?,
        RMessage::Error {
            tag: 2,
            ename: b"cancelled".to_vec(),
        }
    );
    send(&mut stream, TMessage::Stat { tag: 6, fid: 11 })?;
    assert!(matches!(
        receive(&mut stream)?,
        RMessage::Stat { tag: 6, .. }
    ));

    finish_server(stream, join)
}

#[test]
fn version_resets_handler_fids_and_pending_work() -> TestResult<()> {
    let handler = Arc::new(TestHandler::new());
    let (mut stream, join) = start_server(Arc::clone(&handler))?;
    negotiate(&mut stream)?;
    let old = attach(&mut stream, 1, 19)?;
    send(
        &mut stream,
        TMessage::Read {
            tag: 2,
            fid: 19,
            offset: 0,
            count: 32,
        },
    )?;
    handler.wait_started(old.path)?;

    negotiate(&mut stream)?;
    handler.wait_cancelled(old.path)?;
    assert_eq!(handler.resets.load(Ordering::SeqCst), 2);
    let new = attach(&mut stream, 3, 19)?;
    assert_ne!(old, new);
    handler.release(old.path)?;
    handler.wait_finished(old.path)?;
    send(&mut stream, TMessage::Stat { tag: 4, fid: 19 })?;
    assert!(matches!(
        receive(&mut stream)?,
        RMessage::Stat { tag: 4, .. }
    ));

    finish_server(stream, join)
}

#[test]
fn connection_eof_cancels_all_pending_work() -> TestResult<()> {
    let handler = Arc::new(TestHandler::new());
    let (mut stream, join) = start_server(Arc::clone(&handler))?;
    negotiate(&mut stream)?;
    let qid = attach(&mut stream, 1, 29)?;
    send(
        &mut stream,
        TMessage::Read {
            tag: 2,
            fid: 29,
            offset: 0,
            count: 32,
        },
    )?;
    handler.wait_started(qid.path)?;

    drop(stream);
    join.join().map_err(|_| "connection server panicked")??;
    handler.wait_cancelled(qid.path)?;
    assert_eq!(handler.resets.load(Ordering::SeqCst), 2);
    handler.release(qid.path)?;
    handler.wait_finished(qid.path)?;
    Ok(())
}

#[test]
fn synchronous_requests_dispatch_while_async_work_is_parked() -> TestResult<()> {
    let handler = Arc::new(TestHandler::new());
    let (mut stream, join) = start_server(Arc::clone(&handler))?;
    negotiate(&mut stream)?;
    let qid = attach(&mut stream, 1, 23)?;
    send(
        &mut stream,
        TMessage::Read {
            tag: 2,
            fid: 23,
            offset: 0,
            count: 32,
        },
    )?;
    handler.wait_started(qid.path)?;

    send(&mut stream, TMessage::Stat { tag: 3, fid: 23 })?;
    assert!(matches!(
        receive(&mut stream)?,
        RMessage::Stat { tag: 3, .. }
    ));
    handler.release(qid.path)?;
    assert_eq!(
        receive(&mut stream)?,
        RMessage::Read {
            tag: 2,
            data: format!("read-{}", qid.path).into_bytes(),
        }
    );

    finish_server(stream, join)
}

#[test]
fn asynchronous_request_limit_rejects_excess_work_without_spawning_it() -> TestResult<()> {
    let handler = Arc::new(TestHandler::new());
    let (mut stream, join) = start_server_with_config(
        Arc::clone(&handler),
        ServerConfig {
            max_async_requests: 1,
            ..ServerConfig::default()
        },
    )?;
    negotiate(&mut stream)?;
    let qid = attach(&mut stream, 1, 31)?;
    send(
        &mut stream,
        TMessage::Read {
            tag: 2,
            fid: 31,
            offset: 0,
            count: 32,
        },
    )?;
    handler.wait_started(qid.path)?;

    send(
        &mut stream,
        TMessage::Read {
            tag: 3,
            fid: 31,
            offset: 0,
            count: 32,
        },
    )?;
    assert_eq!(
        receive(&mut stream)?,
        RMessage::Error {
            tag: 3,
            ename: b"too many asynchronous 9P requests".to_vec(),
        }
    );

    handler.release(qid.path)?;
    assert_eq!(
        receive(&mut stream)?,
        RMessage::Read {
            tag: 2,
            data: format!("read-{}", qid.path).into_bytes(),
        }
    );
    finish_server(stream, join)
}

#[test]
fn read_flush_storm_does_not_reopen_active_worker_capacity() -> TestResult<()> {
    const ASYNC_LIMIT: usize = 1;
    const STORM_REQUESTS: u16 = 12;

    let handler = Arc::new(TestHandler::new());
    let (mut stream, join) = start_server_with_config(
        Arc::clone(&handler),
        ServerConfig {
            max_async_requests: ASYNC_LIMIT,
            ..ServerConfig::default()
        },
    )?;
    negotiate(&mut stream)?;
    let qid = attach(&mut stream, 1, 37)?;

    send(
        &mut stream,
        TMessage::Read {
            tag: 2,
            fid: 37,
            offset: 0,
            count: 32,
        },
    )?;
    handler.wait_active(ASYNC_LIMIT)?;
    send(&mut stream, TMessage::Flush { tag: 3, oldtag: 2 })?;
    assert_eq!(receive(&mut stream)?, RMessage::Flush { tag: 3 });
    handler.wait_cancelled(qid.path)?;

    for index in 0..STORM_REQUESTS {
        let read_tag = 10 + index * 3;
        let stat_tag = read_tag + 1;
        let flush_tag = read_tag + 2;
        send(
            &mut stream,
            TMessage::Read {
                tag: read_tag,
                fid: 37,
                offset: 0,
                count: 32,
            },
        )?;
        send(
            &mut stream,
            TMessage::Stat {
                tag: stat_tag,
                fid: 37,
            },
        )?;

        let mut rejected = false;
        loop {
            match receive(&mut stream)? {
                RMessage::Error { tag, ename } if tag == read_tag => {
                    assert_eq!(ename, b"too many asynchronous 9P requests");
                    rejected = true;
                }
                RMessage::Stat { tag, .. } if tag == stat_tag => break,
                reply => return Err(format!("unexpected storm reply: {reply:?}").into()),
            }
        }
        if !rejected {
            handler.wait_active_at_least(usize::from(index) + ASYNC_LIMIT + 1)?;
        }

        send(
            &mut stream,
            TMessage::Flush {
                tag: flush_tag,
                oldtag: read_tag,
            },
        )?;
        assert_eq!(receive(&mut stream)?, RMessage::Flush { tag: flush_tag });
    }

    let peak_active = handler.peak_active()?;
    handler.release(qid.path)?;
    handler.wait_active(0)?;
    finish_server(stream, join)?;
    assert!(
        peak_active <= ASYNC_LIMIT,
        "active async handlers exceeded cap: peak {peak_active}, cap {ASYNC_LIMIT}"
    );
    Ok(())
}
