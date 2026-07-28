use std::time::Duration;

use r9p::Fid;

use crate::{Client, ClientSession, Error, Result};

/// One reopened 9P file whose offsets are application-level replay cursors.
///
/// This is deliberately opt-in. On a definitive transport failure, reads and
/// writes are retried at the exact same offset after a fresh attach and
/// walk/open. The file server must therefore make repeated operations at the
/// same offset idempotent. Ordinary mutable files do not satisfy that
/// contract.
pub struct ResumableFid {
    session: ClientSession,
    path: String,
    mode: u8,
    request_timeout: Duration,
    binding: Binding,
    closed: bool,
}

struct Binding {
    client: Client,
    fid: Fid,
}

impl ResumableFid {
    pub fn open(
        session: ClientSession,
        path: impl Into<String>,
        mode: u8,
        request_timeout: Duration,
    ) -> Result<Self> {
        let path = path.into();
        let client = session.snapshot()?;
        let binding = open_binding(&client, &path, mode, request_timeout)?;
        Ok(Self {
            session,
            path,
            mode,
            request_timeout,
            binding,
            closed: false,
        })
    }

    /// Reads at a replay-safe application cursor.
    pub fn read(&mut self, offset: u64, count: u32) -> Result<Vec<u8>> {
        loop {
            match self.binding.client.read_timeout(
                self.binding.fid,
                offset,
                count,
                self.request_timeout,
            ) {
                Ok(bytes) => return Ok(bytes),
                Err(error) if error.is_definitive_transport_failure() => {
                    self.reopen_after_failure()?;
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Writes at a replay-safe application cursor.
    ///
    /// A response can be lost after the server committed the bytes. Recovery
    /// therefore repeats the write at the same offset. The remote file must
    /// recognize that repetition and return the original success without
    /// applying the bytes twice.
    pub fn write(&mut self, offset: u64, data: &[u8]) -> Result<u32> {
        loop {
            match self.binding.client.write_timeout(
                self.binding.fid,
                offset,
                data,
                self.request_timeout,
            ) {
                Ok(count) => return Ok(count),
                Err(error) if error.is_definitive_transport_failure() => {
                    self.reopen_after_failure()?;
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub fn close(mut self) -> Result<()> {
        self.clunk()
    }

    fn reopen_after_failure(&mut self) -> Result<()> {
        let failed = self.binding.client.clone();
        let client = self.session.reconnect_after(&failed)?;
        self.binding = open_binding(&client, &self.path, self.mode, self.request_timeout)?;
        Ok(())
    }

    fn clunk(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        self.binding
            .client
            .clunk_timeout(self.binding.fid, self.request_timeout)
    }
}

fn open_binding(client: &Client, path: &str, mode: u8, timeout: Duration) -> Result<Binding> {
    let fid = client.walk_path_timeout(path, timeout)?;
    if let Err(error) = client.open_timeout(fid, mode, timeout) {
        let _ = client.clunk_timeout(fid, timeout);
        return Err(error);
    }
    Ok(Binding {
        client: client.clone(),
        fid,
    })
}

impl Drop for ResumableFid {
    fn drop(&mut self) {
        let _ = self.clunk();
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{self, Read, Write},
        net::{Shutdown, TcpListener, TcpStream},
        sync::{Arc, Mutex},
        thread,
    };

    use r9p::{
        codec,
        error::{Error as P9Error, Result as P9Result},
        qid::{Qid, DMDIR},
        server::{FileTree, OpenFile, ReadData, Server, ServerConfig},
        stat::Stat,
        ORDWR,
    };

    use super::*;

    #[test]
    fn reconnect_replays_offsets_without_reapplying_input() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        let state = Arc::new(Mutex::new(ReplayState::default()));
        let server_state = Arc::clone(&state);
        let server = thread::spawn(move || {
            let (first, _) = listener.accept().expect("first connection");
            let shutdown = first.try_clone().expect("shutdown stream");
            let _ = serve(
                first,
                ReplayTree {
                    state: Arc::clone(&server_state),
                    shutdown_after_first_write: Some(shutdown),
                },
            );
            let (second, _) = listener.accept().expect("replacement connection");
            serve(
                second,
                ReplayTree {
                    state: server_state,
                    shutdown_after_first_write: None,
                },
            )
            .expect("replacement server");
        });

        let session = ClientSession::connect(
            &crate::ConnectionConfig {
                address: address.to_string(),
                uname: "test".to_string(),
                aname: "/".to_string(),
                msize: 8192,
                auth_config: None,
                authorities: crate::AuthorityBindings::default(),
            },
            Duration::from_secs(1),
        )
        .expect("session");
        let mut reader =
            ResumableFid::open(session.clone(), "/stream", ORDWR, Duration::from_secs(1))
                .expect("reader");
        let mut writer =
            ResumableFid::open(session.clone(), "/stream", ORDWR, Duration::from_secs(1))
                .expect("writer");

        assert_eq!(writer.write(0, b"hello").expect("replayed write"), 5);
        assert_eq!(reader.read(0, 64).expect("replayed read"), b"reply");
        assert_eq!(
            state.lock().expect("state").input.as_slice(),
            b"hello",
            "the uncertain first write must not be applied twice"
        );

        writer.close().expect("writer close");
        reader.close().expect("reader close");
        session.shutdown().expect("session shutdown");
        server.join().expect("server thread");
    }

    #[derive(Default)]
    struct ReplayState {
        input: Vec<u8>,
    }

    struct ReplayTree {
        state: Arc<Mutex<ReplayState>>,
        shutdown_after_first_write: Option<TcpStream>,
    }

    impl FileTree for ReplayTree {
        fn attach(&mut self, _fid: Fid, _uname: &[u8], _aname: &[u8]) -> P9Result<Qid> {
            Ok(Qid::dir(1))
        }

        fn walk(
            &mut self,
            _fid: Fid,
            _newfid: Fid,
            start: Qid,
            names: &[Vec<u8>],
        ) -> P9Result<Vec<Qid>> {
            if names.is_empty() {
                return Ok(Vec::new());
            }
            if start == Qid::dir(1) && names.len() == 1 && names[0].as_slice() == b"stream" {
                Ok(vec![Qid::file(2)])
            } else {
                Err(P9Error::from("file does not exist"))
            }
        }

        fn open(&mut self, _fid: Fid, qid: Qid, mode: u8) -> P9Result<OpenFile> {
            if qid != Qid::file(2) || mode != ORDWR {
                return Err(P9Error::from("operation not permitted"));
            }
            Ok(OpenFile { qid, iounit: 4096 })
        }

        fn read(&mut self, _fid: Fid, qid: Qid, offset: u64, count: u32) -> P9Result<ReadData> {
            if qid != Qid::file(2) {
                return Err(P9Error::from("bad fid"));
            }
            let output = b"reply";
            let start = usize::try_from(offset)
                .unwrap_or(usize::MAX)
                .min(output.len());
            let end = start
                .saturating_add(usize::try_from(count).unwrap_or(usize::MAX))
                .min(output.len());
            Ok(ReadData::Bytes(output[start..end].to_vec()))
        }

        fn write(&mut self, _fid: Fid, qid: Qid, offset: u64, data: &[u8]) -> P9Result<u32> {
            if qid != Qid::file(2) || offset != 0 {
                return Err(P9Error::from("input sequence conflict"));
            }
            let mut state = self.state.lock().expect("state");
            if state.input.is_empty() {
                state.input.extend_from_slice(data);
            } else if state.input != data {
                return Err(P9Error::from("input replay conflict"));
            }
            drop(state);
            if let Some(stream) = self.shutdown_after_first_write.take() {
                let _ = stream.shutdown(Shutdown::Both);
            }
            u32::try_from(data.len()).map_err(|_| P9Error::from("write too large"))
        }

        fn stat(&mut self, qid: Qid) -> P9Result<Stat> {
            if qid == Qid::file(2) {
                Ok(Stat::new("stream", qid, 0o660))
            } else {
                Ok(Stat::new(".", Qid::dir(1), DMDIR | 0o555))
            }
        }
    }

    fn serve(mut stream: impl Read + Write, tree: impl FileTree) -> io::Result<()> {
        let mut server = Server::with_config(tree, ServerConfig::default());
        while let Some(message) =
            codec::read_tmessage_checked(&mut stream, server.session().msize())
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?
        {
            let response = server.handle(message);
            codec::write_rmessage_checked(&mut stream, server.session().msize(), &response)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        }
        Ok(())
    }
}
