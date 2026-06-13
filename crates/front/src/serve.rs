use crate::Front;
use crate::ReadTarget;
use r9p::codec;
use r9p::error::{Error, Result};
use r9p::fid::NOFID;
use r9p::flush::{FlushOutcome, RequestKey};
use r9p::message::{RMessage, TMessage};
use r9p::server::{
    FileTree, Server, ServerCompletion, ServerConfig, ServerEvent, ServerRequest, ServerRequestKind,
};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

pub struct ServeHandle {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    join: Mutex<Option<thread::JoinHandle<()>>>,
}

impl ServeHandle {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
    }

    pub fn join(&self) {
        if let Ok(mut join) = self.join.lock() {
            if let Some(join) = join.take() {
                let _ = join.join();
            }
        }
    }

    pub fn shutdown(&self) {
        self.stop();
        self.join();
    }
}

impl Front {
    pub fn serve_tcp(&self, bind: &str) -> Result<ServeHandle> {
        let listener = TcpListener::bind(bind)
            .map_err(|error| Error::new(format!("front bind {bind}: {error}")))?;
        let addr = listener
            .local_addr()
            .map_err(|error| Error::new(format!("front local addr: {error}")))?;
        let stop = Arc::new(AtomicBool::new(false));
        let accept_stop = Arc::clone(&stop);
        let front = self.clone();
        let join = thread::spawn(move || {
            for stream in listener.incoming() {
                if accept_stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let connection_front = front.clone();
                let connection_stop = Arc::clone(&accept_stop);
                thread::spawn(move || {
                    let _ = serve_connection(&connection_front, stream, connection_stop);
                });
            }
        });
        Ok(ServeHandle {
            addr,
            stop,
            join: Mutex::new(Some(join)),
        })
    }
}

fn serve_connection(front: &Front, stream: TcpStream, stop: Arc<AtomicBool>) -> Result<()> {
    let mut reader = stream
        .try_clone()
        .map_err(|error| Error::new(format!("clone 9P stream: {error}")))?;
    let writer = Arc::new(Mutex::new(stream));
    let server = Arc::new(Mutex::new(Server::with_config((), ServerConfig::default())));
    let tree = Arc::new(Mutex::new(front.tree()));
    let cancels = Arc::new(Mutex::new(BTreeMap::new()));
    loop {
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        let message = match read_tmessage(&mut reader) {
            Ok(message) => message,
            Err(_) => return Ok(()),
        };
        let event = {
            let mut server = server
                .lock()
                .map_err(|_| Error::from_static("front server session poisoned"))?;
            server.admit(message)
        };
        match event {
            ServerEvent::Reply(reply) => {
                write_reply(&server, &writer, &reply)?;
            }
            ServerEvent::Flush { reply, outcome } => {
                cancel_request(front, &cancels, outcome)?;
                write_reply(&server, &writer, &reply)?;
            }
            ServerEvent::Dispatch(request) if request_is_async(&request) => {
                let cancel = Arc::new(AtomicBool::new(false));
                cancels
                    .lock()
                    .map_err(|_| Error::from_static("front cancel map poisoned"))?
                    .insert(request.key, Arc::clone(&cancel));
                let server = Arc::clone(&server);
                let writer = Arc::clone(&writer);
                let tree = Arc::clone(&tree);
                let cancels = Arc::clone(&cancels);
                thread::spawn(move || {
                    let key = request.key;
                    let completion = perform_request(&tree, &request, Some(cancel));
                    let reply = match server.lock() {
                        Ok(mut server) => server.complete(request, completion),
                        Err(_) => None,
                    };
                    if let Ok(mut cancels) = cancels.lock() {
                        cancels.remove(&key);
                    }
                    if let Some(reply) = reply {
                        let _ = write_reply(&server, &writer, &reply);
                    }
                });
            }
            ServerEvent::Dispatch(request) => {
                let completion = perform_request(&tree, &request, None);
                let reply = {
                    let mut server = server
                        .lock()
                        .map_err(|_| Error::from_static("front server session poisoned"))?;
                    server.complete(request, completion)
                };
                if let Some(reply) = reply {
                    write_reply(&server, &writer, &reply)?;
                }
            }
        }
    }
}

type CancelMap = Arc<Mutex<BTreeMap<RequestKey, Arc<AtomicBool>>>>;

fn cancel_request(front: &Front, cancels: &CancelMap, outcome: FlushOutcome) -> Result<()> {
    if let FlushOutcome::Cancelled(key) = outcome {
        if let Some(cancel) = cancels
            .lock()
            .map_err(|_| Error::from_static("front cancel map poisoned"))?
            .remove(&key)
        {
            cancel.store(true, Ordering::SeqCst);
            front.wake_readers();
        }
    }
    Ok(())
}

fn request_is_async(request: &ServerRequest) -> bool {
    matches!(request.kind, ServerRequestKind::Read { .. })
}

fn perform_request(
    tree: &Arc<Mutex<crate::FrontTree>>,
    request: &ServerRequest,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<ServerCompletion> {
    match &request.kind {
        ServerRequestKind::Auth { afid, uname, aname } => {
            let mut tree = tree
                .lock()
                .map_err(|_| Error::from_static("front tree poisoned"))?;
            tree.auth(*afid, uname, aname)
                .map(|qid| ServerCompletion::Auth { qid })
        }
        ServerRequestKind::Attach {
            fid,
            afid,
            uname,
            aname,
        } => {
            let mut tree = tree
                .lock()
                .map_err(|_| Error::from_static("front tree poisoned"))?;
            let qid = if *afid == NOFID {
                tree.attach(*fid, uname, aname)?
            } else {
                tree.attach_with_auth(*fid, *afid, uname, aname)?
            };
            Ok(ServerCompletion::Attach { qid })
        }
        ServerRequestKind::Walk {
            fid,
            newfid,
            wnames,
            start,
        } => {
            let mut tree = tree
                .lock()
                .map_err(|_| Error::from_static("front tree poisoned"))?;
            tree.walk(*fid, *newfid, *start, wnames)
                .map(|qids| ServerCompletion::Walk { qids })
        }
        ServerRequestKind::Open { fid, qid, mode } => {
            let mut tree = tree
                .lock()
                .map_err(|_| Error::from_static("front tree poisoned"))?;
            tree.open(*fid, *qid, *mode).map(ServerCompletion::Open)
        }
        ServerRequestKind::Create {
            fid,
            qid,
            name,
            perm,
            mode,
        } => {
            let mut tree = tree
                .lock()
                .map_err(|_| Error::from_static("front tree poisoned"))?;
            tree.create(*fid, *qid, name, *perm, *mode)
                .map(ServerCompletion::Create)
        }
        ServerRequestKind::Read {
            fid,
            qid: _,
            offset,
            count,
        } => {
            let (front, target) = {
                let tree = tree
                    .lock()
                    .map_err(|_| Error::from_static("front tree poisoned"))?;
                (tree.front(), tree.read_target(*fid)?)
            };
            let read = match target {
                ReadTarget::Node(id) => front.read_node(id, *offset, *count, cancel.as_deref()),
                ReadTarget::Rpc(request_id) => {
                    front.rpc_read(request_id, *offset, *count, cancel.as_deref())
                }
            };
            read.map(ServerCompletion::Read)
        }
        ServerRequestKind::Write {
            fid,
            qid,
            offset,
            data,
        } => {
            let mut tree = tree
                .lock()
                .map_err(|_| Error::from_static("front tree poisoned"))?;
            tree.write(*fid, *qid, *offset, data)
                .map(|count| ServerCompletion::Write { count })
        }
        ServerRequestKind::Clunk { fid, qid } => {
            let mut tree = tree
                .lock()
                .map_err(|_| Error::from_static("front tree poisoned"))?;
            tree.clunk(*fid, *qid).map(|()| ServerCompletion::Clunk)
        }
        ServerRequestKind::Remove { fid, qid } => {
            let mut tree = tree
                .lock()
                .map_err(|_| Error::from_static("front tree poisoned"))?;
            tree.remove(*fid, *qid).map(|()| ServerCompletion::Remove)
        }
        ServerRequestKind::Stat { qid, .. } => {
            let mut tree = tree
                .lock()
                .map_err(|_| Error::from_static("front tree poisoned"))?;
            tree.stat(*qid).map(|stat| ServerCompletion::Stat { stat })
        }
        ServerRequestKind::Wstat { fid, qid, stat } => {
            let mut tree = tree
                .lock()
                .map_err(|_| Error::from_static("front tree poisoned"))?;
            tree.wstat(*fid, *qid, stat)
                .map(|()| ServerCompletion::Wstat)
        }
    }
}

fn write_reply(
    server: &Arc<Mutex<Server<()>>>,
    writer: &Arc<Mutex<TcpStream>>,
    reply: &RMessage,
) -> Result<()> {
    let msize = server
        .lock()
        .map_err(|_| Error::from_static("front server session poisoned"))?
        .session()
        .msize();
    let frame = codec::encode_rmessage_checked(reply, msize)?;
    let mut writer = writer
        .lock()
        .map_err(|_| Error::from_static("front writer poisoned"))?;
    if writer.write_all(&frame).is_err() || writer.flush().is_err() {
        return Err(Error::from_static("write 9P reply"));
    }
    Ok(())
}

fn read_tmessage(stream: &mut impl Read) -> Result<TMessage> {
    let mut prefix = [0_u8; 4];
    stream
        .read_exact(&mut prefix)
        .map_err(|error| Error::new(format!("read 9P frame size: {error}")))?;
    let size = u32::from_le_bytes(prefix);
    if size < codec::FRAME_HEADER_SIZE {
        return Err(Error::from_static("short 9P frame"));
    }
    if size > codec::MAX_MSIZE {
        return Err(Error::from_static("oversized 9P frame"));
    }
    let rest_len =
        usize::try_from(size - 4).map_err(|_| Error::from_static("oversized 9P frame"))?;
    let mut frame = Vec::with_capacity(rest_len + 4);
    frame.extend(prefix);
    frame.resize(rest_len + 4, 0);
    stream
        .read_exact(&mut frame[4..])
        .map_err(|error| Error::new(format!("read 9P frame body: {error}")))?;
    codec::decode_tmessage(&frame)
}
