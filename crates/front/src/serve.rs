use crate::Front;
use r9p::codec;
use r9p::error::{Error, Result};
use r9p::message::TMessage;
use r9p::server::{Server, ServerConfig};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

pub struct ServeHandle {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
}

impl ServeHandle {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
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
        thread::spawn(move || {
            for stream in listener.incoming() {
                if accept_stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let connection_front = front.clone();
                thread::spawn(move || {
                    let _ = serve_connection(&connection_front, stream);
                });
            }
        });
        Ok(ServeHandle { addr, stop })
    }
}

fn serve_connection(front: &Front, mut stream: TcpStream) -> Result<()> {
    let mut server = Server::with_config(front.tree(), ServerConfig::default());
    loop {
        let message = match read_tmessage(&mut stream) {
            Ok(message) => message,
            Err(_) => return Ok(()),
        };
        let reply = server.handle(message);
        let frame = codec::encode_rmessage_checked(&reply, server.session().msize())?;
        if stream.write_all(&frame).is_err() || stream.flush().is_err() {
            return Ok(());
        }
    }
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
    let rest_len = usize::try_from(size - 4)
        .map_err(|_| Error::from_static("oversized 9P frame"))?;
    let mut frame = Vec::with_capacity(rest_len + 4);
    frame.extend(prefix);
    frame.resize(rest_len + 4, 0);
    stream
        .read_exact(&mut frame[4..])
        .map_err(|error| Error::new(format!("read 9P frame body: {error}")))?;
    codec::decode_tmessage(&frame)
}
