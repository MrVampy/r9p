mod client;
mod reader;
mod util;

pub use client::{MultiplexedClient, PendingCall};

use std::{
    io::{self, Read, Write},
    net::{Shutdown, TcpStream},
};

#[cfg(unix)]
use std::os::unix::net::UnixStream;

pub trait MultiplexTransport: Read + Write + Send + 'static {
    fn try_clone_transport(&self) -> io::Result<Self>
    where
        Self: Sized;

    fn shutdown_transport(&self) -> io::Result<()>;
}

impl MultiplexTransport for TcpStream {
    fn try_clone_transport(&self) -> io::Result<Self> {
        self.try_clone()
    }

    fn shutdown_transport(&self) -> io::Result<()> {
        self.shutdown(Shutdown::Both)
    }
}

#[cfg(unix)]
impl MultiplexTransport for UnixStream {
    fn try_clone_transport(&self) -> io::Result<Self> {
        self.try_clone()
    }

    fn shutdown_transport(&self) -> io::Result<()> {
        self.shutdown(Shutdown::Both)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        codec,
        error::{Error, Result},
        fid::NOFID,
        message::{RMessage, TMessage},
        qid::Qid,
        stat::Stat,
    };
    use std::io::Read;
    use std::net::{TcpListener, TcpStream};
    use std::sync::{mpsc, Arc, Barrier};
    use std::thread;

    use super::client::pending_for_test;
    use super::reader::write_response;
    use super::util::io_error;

    #[test]
    fn concurrent_calls_are_demultiplexed_by_tag() -> Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .map_err(|error| io_error("bind test listener", error))?;
        let address = listener
            .local_addr()
            .map_err(|error| io_error("read listener address", error))?;
        let server = thread::spawn(move || {
            let (stream, _) = listener
                .accept()
                .map_err(|error| io_error("accept test connection", error))?;
            scripted_out_of_order_server(stream)
        });
        let client = Arc::new(MultiplexedClient::connect(
            TcpStream::connect(address).map_err(|error| io_error("connect test client", error))?,
            "glenda",
            "",
            8192,
        )?);
        let root = client.root_fid();
        let barrier = Arc::new(Barrier::new(3));

        let read_client = Arc::clone(&client);
        let read_barrier = Arc::clone(&barrier);
        let read_thread = thread::spawn(move || {
            read_barrier.wait();
            read_client.read(root, 0, 100)
        });

        let stat_client = Arc::clone(&client);
        let stat_barrier = Arc::clone(&barrier);
        let stat_thread = thread::spawn(move || {
            stat_barrier.wait();
            stat_client.stat(root)
        });

        barrier.wait();
        let stat = stat_thread
            .join()
            .map_err(|_| Error::from("stat worker panicked"))??;
        let data = read_thread
            .join()
            .map_err(|_| Error::from("read worker panicked"))??;
        assert_eq!(stat.name, b".".to_vec());
        assert_eq!(data, b"read after stat\n".to_vec());
        server
            .join()
            .map_err(|_| Error::from("server worker panicked"))??;
        Ok(())
    }

    #[test]
    fn flush_releases_original_waiter() -> Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .map_err(|error| io_error("bind test listener", error))?;
        let address = listener
            .local_addr()
            .map_err(|error| io_error("read listener address", error))?;
        let (done_sender, done_receiver) = mpsc::channel();
        let server = thread::spawn(move || {
            let (stream, _) = listener
                .accept()
                .map_err(|error| io_error("accept test connection", error))?;
            scripted_flush_server(stream, done_receiver)
        });
        let client = MultiplexedClient::connect(
            TcpStream::connect(address).map_err(|error| io_error("connect test client", error))?,
            "glenda",
            "",
            8192,
        )?;
        let pending = client.submit(|protocol| protocol.read(client.root_fid(), 0, 100))?;
        let oldtag = pending.tag();
        client.flush_tag(oldtag)?;
        let error = pending.wait().err().ok_or("flushed request completed")?;
        assert_eq!(error.message(), b"9P request flushed");
        done_sender
            .send(())
            .map_err(|_| Error::from("flush server stopped early"))?;
        server
            .join()
            .map_err(|_| Error::from("server worker panicked"))??;
        Ok(())
    }

    fn scripted_out_of_order_server(mut stream: TcpStream) -> Result<()> {
        handshake(&mut stream)?;

        let first = read_tmessage(&mut stream)?;
        let second = read_tmessage(&mut stream)?;
        let mut read_tag = None;
        let mut stat_tag = None;
        for message in [first, second] {
            match message {
                TMessage::Read { tag, .. } => read_tag = Some(tag),
                TMessage::Stat { tag, .. } => stat_tag = Some(tag),
                other => return Err(Error::from(format!("unexpected request: {other:?}"))),
            }
        }

        write_response(
            &mut stream,
            &RMessage::Stat {
                tag: stat_tag.ok_or("missing Tstat")?,
                stat: Stat::new(".", Qid::dir(1), crate::qid::DMDIR | 0o500),
            },
        )?;
        write_response(
            &mut stream,
            &RMessage::Read {
                tag: read_tag.ok_or("missing Tread")?,
                data: b"read after stat\n".to_vec(),
            },
        )?;
        Ok(())
    }

    fn scripted_flush_server(mut stream: TcpStream, done: mpsc::Receiver<()>) -> Result<()> {
        handshake(&mut stream)?;
        let read = read_tmessage(&mut stream)?;
        let read_tag = match read {
            TMessage::Read { tag, .. } => tag,
            other => return Err(Error::from(format!("expected Tread, got {other:?}"))),
        };
        let flush = read_tmessage(&mut stream)?;
        let flush_tag = match flush {
            TMessage::Flush { tag, oldtag } if oldtag == read_tag => tag,
            other => return Err(Error::from(format!("expected Tflush, got {other:?}"))),
        };
        write_response(&mut stream, &RMessage::Flush { tag: flush_tag })?;
        done.recv()
            .map_err(|_| Error::from("flush test ended before server release"))?;
        Ok(())
    }

    fn handshake(stream: &mut TcpStream) -> Result<()> {
        let version = read_tmessage(stream)?;
        match version {
            TMessage::Version { tag, msize, .. } => write_response(
                stream,
                &RMessage::Version {
                    tag,
                    msize,
                    version: b"9P2000".to_vec(),
                },
            )?,
            other => return Err(Error::from(format!("expected Tversion, got {other:?}"))),
        }
        let attach = read_tmessage(stream)?;
        match attach {
            TMessage::Attach { tag, fid, afid, .. } if fid != NOFID && afid == NOFID => {
                write_response(
                    &mut *stream,
                    &RMessage::Attach {
                        tag,
                        qid: Qid::dir(1),
                    },
                )?
            }
            other => return Err(Error::from(format!("expected Tattach, got {other:?}"))),
        }
        Ok(())
    }

    fn read_tmessage(stream: &mut TcpStream) -> Result<TMessage> {
        let mut prefix = [0_u8; 4];
        stream
            .read_exact(&mut prefix)
            .map_err(|error| io_error("read T-message size", error))?;
        let size = u32::from_le_bytes(prefix);
        let rest_len = usize::try_from(size - 4).map_err(|_| Error::from("oversized 9P frame"))?;
        let mut frame = Vec::with_capacity(usize::try_from(size).unwrap_or(rest_len + 4));
        frame.extend(prefix);
        frame.resize(rest_len + 4, 0);
        stream
            .read_exact(&mut frame[4..])
            .map_err(|error| io_error("read T-message body", error))?;
        codec::decode_tmessage(&frame)
            .map_err(|error| Error::from(format!("decode 9P T-message: {error}")))
    }

    #[test]
    fn pending_wait_reports_closed_reader() {
        let (sender, receiver) = mpsc::channel();
        drop(sender);
        let pending = pending_for_test(7, receiver);
        let error = pending.wait().expect_err("closed reader should fail");
        assert_eq!(error.message(), b"9P reader stopped before response");
    }
}
