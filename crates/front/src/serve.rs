use crate::Front;
use crate::ReadTarget;
use r9p::error::{Error, Result};
use r9p::fid::{Fid, NOFID};
use r9p::server::{
    serve_connection as serve_protocol_connection, ConnectionHandler, FileTree, ReadData,
    ServerCompletion, ServerConfig, ServerRequest, ServerRequestKind,
};
use r9p_auth::{authenticate_server, ServerConfig as SessionAuthConfig};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub use r9p::server::ConnectionStream as FrontServeStream;

const AUTH_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

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
    pub fn connection_handler(&self) -> FrontConnectionHandler {
        FrontConnectionHandler::new(self.clone())
    }

    pub fn serve_stream<S>(&self, stream: S) -> Result<()>
    where
        S: FrontServeStream,
    {
        serve_front_connection(self, stream, None)
    }

    pub fn serve_tcp(&self, bind: &str) -> Result<ServeHandle> {
        self.serve_tcp_with_auth(bind, None)
    }

    pub fn serve_tcp_authenticated(
        &self,
        bind: &str,
        auth_config_path: &Path,
    ) -> Result<ServeHandle> {
        let auth = SessionAuthConfig::read(auth_config_path)?;
        self.serve_tcp_with_auth(bind, Some(auth))
    }

    fn serve_tcp_with_auth(
        &self,
        bind: &str,
        auth: Option<SessionAuthConfig>,
    ) -> Result<ServeHandle> {
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
                let connection_auth = auth.clone();
                thread::spawn(move || match connection_auth {
                    Some(auth) => {
                        let Ok(session) =
                            authenticate_server(stream, &auth, AUTH_HANDSHAKE_TIMEOUT)
                        else {
                            return;
                        };
                        let session_uname = Some(session.peer.principal().as_bytes().to_vec());
                        let _ = serve_front_connection(
                            &connection_front,
                            StoppableStream::new(session.stream, connection_stop),
                            session_uname,
                        );
                    }
                    None => {
                        let _ = serve_front_connection(
                            &connection_front,
                            StoppableStream::new(stream, connection_stop),
                            None,
                        );
                    }
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

struct StoppableStream<S> {
    inner: S,
    stop: Arc<AtomicBool>,
}

impl<S> StoppableStream<S> {
    fn new(inner: S, stop: Arc<AtomicBool>) -> Self {
        Self { inner, stop }
    }
}

impl<S: Read> Read for StoppableStream<S> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.stop.load(Ordering::SeqCst) {
            Ok(0)
        } else {
            self.inner.read(buffer)
        }
    }
}

impl<S: Write> Write for StoppableStream<S> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.inner.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<S: FrontServeStream> FrontServeStream for StoppableStream<S> {
    fn try_clone_stream(&self) -> io::Result<Self> {
        Ok(Self::new(
            self.inner.try_clone_stream()?,
            Arc::clone(&self.stop),
        ))
    }
}

fn serve_front_connection<S>(front: &Front, stream: S, session_uname: Option<Vec<u8>>) -> Result<()>
where
    S: FrontServeStream,
{
    let max_msize = front.max_msize()?;
    serve_protocol_connection(
        stream,
        ServerConfig {
            default_msize: max_msize,
            max_msize,
            session_uname,
            ..ServerConfig::default()
        },
        Arc::new(front.connection_handler()),
    )
}

pub struct FrontConnectionHandler {
    front: Front,
    tree: Mutex<crate::FrontTree>,
}

impl FrontConnectionHandler {
    fn new(front: Front) -> Self {
        let tree = front.tree();
        Self {
            front,
            tree: Mutex::new(tree),
        }
    }
}

impl ConnectionHandler for FrontConnectionHandler {
    fn perform(
        &self,
        request: &ServerRequest,
        cancel: Option<&AtomicBool>,
    ) -> Result<ServerCompletion> {
        perform_request(&self.tree, request, cancel)
    }

    fn is_async(&self, request: &ServerRequest) -> bool {
        matches!(request.kind, ServerRequestKind::Read { .. })
    }

    fn cancellation_fid(&self, request: &ServerRequest) -> Option<Fid> {
        match &request.kind {
            ServerRequestKind::Read { fid, .. } => Some(*fid),
            _ => None,
        }
    }

    fn reset(&self) -> Result<()> {
        let mut tree = self
            .tree
            .lock()
            .map_err(|_| Error::from_static("front tree poisoned"))?;
        tree.reset()
    }

    fn wake_after_cancel(&self) {
        self.front.wake_readers();
    }
}

fn perform_request(
    tree: &Mutex<crate::FrontTree>,
    request: &ServerRequest,
    cancel: Option<&AtomicBool>,
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
                let mut tree = tree
                    .lock()
                    .map_err(|_| Error::from_static("front tree poisoned"))?;
                (tree.front(), tree.read_target_at(*fid, *offset, *count)?)
            };
            let read = match target {
                ReadTarget::Node(id) => front.read_node(id, *offset, *count, cancel),
                ReadTarget::Directory(stats) => Ok(ReadData::Directory(stats)),
                ReadTarget::Response(request_id, response_offset, consume) => {
                    front.response_read(request_id, response_offset, *count, cancel, consume)
                }
                ReadTarget::DirectoryResponse {
                    request_id,
                    fid,
                    node,
                } => {
                    let response = front.directory_response(request_id, cancel);
                    let mut tree = tree
                        .lock()
                        .map_err(|_| Error::from_static("front tree poisoned"))?;
                    tree.apply_directory_response(fid, node, request_id, response)
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
        ServerRequestKind::RenameAt {
            olddirfid,
            olddir_qid,
            oldname,
            newdirfid,
            newdir_qid,
            newname,
        } => {
            let mut tree = tree
                .lock()
                .map_err(|_| Error::from_static("front tree poisoned"))?;
            tree.rename_at(
                *olddirfid,
                *olddir_qid,
                oldname,
                *newdirfid,
                *newdir_qid,
                newname,
            )
            .map(|()| ServerCompletion::RenameAt)
        }
        ServerRequestKind::Referrals { fid, qid } => {
            let mut tree = tree
                .lock()
                .map_err(|_| Error::from_static("front tree poisoned"))?;
            tree.referrals(*fid, *qid)
                .map(|referrals| ServerCompletion::Referrals { referrals })
        }
    }
}
