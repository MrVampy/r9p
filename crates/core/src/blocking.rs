use crate::{
    client::{Client as ProtocolClient, ClientResponse, Completion, Op},
    codec,
    error::ENOTDIR,
    error::{Error, Result},
    fid::{Fid, NOFID},
    message::TMessage,
    qid::{Qid, DMDIR},
    stat::{decode_dir_entries, Stat},
};
use std::{
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    time::Duration,
};

#[cfg(unix)]
use std::{os::unix::net::UnixStream, path::Path};

pub const OREAD: u8 = 0;
pub const OWRITE: u8 = 1;
pub const ORDWR: u8 = 2;
pub const OEXEC: u8 = 3;
pub const OTRUNC: u8 = 0x10;
pub const ORCLOSE: u8 = 0x40;
pub const DEFAULT_READ_CHUNK: u32 = 65_536;

pub trait ReadWrite: Read + Write + Send {}

impl<T: Read + Write + Send> ReadWrite for T {}

pub type BoxedClient = Client<Box<dyn ReadWrite>>;

/// Independent finite timeouts for a blocking TCP connection and its I/O.
///
/// Every duration must be greater than zero. `connect_timeout` applies to each
/// socket address produced by name resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TcpConnectionTimeouts {
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
    pub write_timeout: Duration,
}

impl TcpConnectionTimeouts {
    pub const fn new(
        connect_timeout: Duration,
        read_timeout: Duration,
        write_timeout: Duration,
    ) -> Self {
        Self {
            connect_timeout,
            read_timeout,
            write_timeout,
        }
    }

    fn validate(self) -> Result<Self> {
        validate_nonzero_timeout("TCP connect", self.connect_timeout)?;
        validate_nonzero_timeout("TCP read", self.read_timeout)?;
        validate_nonzero_timeout("TCP write", self.write_timeout)?;
        Ok(self)
    }
}

pub struct Client<S> {
    stream: S,
    protocol: ProtocolClient,
    root_fid: Fid,
    root_qid: Qid,
}

impl Client<TcpStream> {
    pub fn connect_tcp(address: &str, uname: &str, aname: &str, msize: u32) -> Result<Self> {
        let socket = parse_tcp_address(address)?;
        let stream = TcpStream::connect(&socket)
            .map_err(|error| io_error(format!("connect {socket}"), error))?;
        stream
            .set_nodelay(true)
            .map_err(|error| io_error("set TCP_NODELAY", error))?;
        Self::connect(stream, uname, aname, msize)
    }

    /// Connects over TCP with finite transport timeouts installed before the
    /// 9P version and attach handshake.
    pub fn connect_tcp_with_timeouts(
        address: &str,
        uname: &str,
        aname: &str,
        msize: u32,
        timeouts: TcpConnectionTimeouts,
    ) -> Result<Self> {
        let socket = parse_tcp_address(address)?;
        let stream = connect_tcp_stream_with_timeouts(&socket, timeouts)?;
        Self::connect(stream, uname, aname, msize)
    }
}

#[cfg(unix)]
impl Client<UnixStream> {
    pub fn connect_unix(path: &Path, uname: &str, aname: &str, msize: u32) -> Result<Self> {
        let stream = UnixStream::connect(path)
            .map_err(|error| io_error(format!("connect {}", path.display()), error))?;
        Self::connect(stream, uname, aname, msize)
    }
}

impl<S: Read + Write> Client<S> {
    pub fn connect(stream: S, uname: &str, aname: &str, msize: u32) -> Result<Self> {
        let mut client = Self {
            stream,
            protocol: ProtocolClient::new(),
            root_fid: NOFID,
            root_qid: Qid::file(0),
        };
        let version_request = client.protocol.version_request(msize);
        match client.call_message(version_request)? {
            ClientResponse::Completion {
                completion: Completion::Version { version, .. },
                ..
            } if version == b"9P2000" => {}
            ClientResponse::Completion {
                completion: Completion::Version { version, .. },
                ..
            } => {
                return Err(Error::from(format!(
                    "server negotiated unsupported version {}",
                    String::from_utf8_lossy(&version)
                )));
            }
            other => return Err(unexpected("Rversion", other)),
        }

        let attach = client
            .protocol
            .attach(uname.as_bytes().to_vec(), aname.as_bytes().to_vec())
            .map_err(protocol_error)?;
        let fid = op_fid(&attach)?;
        match client.call_op(attach)? {
            Completion::Attach { qid } => {
                client.root_fid = fid;
                client.root_qid = qid;
                Ok(client)
            }
            other => Err(unexpected("Rattach", other)),
        }
    }

    pub fn version(&self) -> &[u8] {
        self.protocol.version()
    }

    pub fn root_fid(&self) -> Fid {
        self.root_fid
    }

    pub fn root_qid(&self) -> Qid {
        self.root_qid
    }

    pub fn msize(&self) -> u32 {
        self.protocol.msize()
    }

    pub fn max_write_payload(&self) -> u32 {
        codec::max_write_payload(self.protocol.msize()).max(1)
    }

    pub fn clone_fid(&mut self, fid: Fid) -> Result<Fid> {
        self.walk(fid, &[])
    }

    pub fn walk_path(&mut self, path: &str) -> Result<Fid> {
        let names = path_names(path);
        if names.is_empty() {
            return self.clone_fid(self.root_fid);
        }
        self.walk(self.root_fid, &names)
    }

    pub fn walk_one(&mut self, fid: Fid, name: &[u8]) -> Result<Fid> {
        self.walk(fid, &[name.to_vec()])
    }

    pub fn walk(&mut self, fid: Fid, names: &[Vec<u8>]) -> Result<Fid> {
        let op = self
            .protocol
            .walk(fid, names.to_vec())
            .map_err(protocol_error)?;
        let newfid = op_fid(&op)?;
        match self.call_op(op)? {
            Completion::Walk { qids } if qids.len() == names.len() => Ok(newfid),
            Completion::Walk { .. } => {
                let _ = self.clunk(newfid);
                Err(Error::from("partial walk"))
            }
            other => {
                let _ = self.clunk(newfid);
                Err(unexpected("Rwalk", other))
            }
        }
    }

    pub fn open(&mut self, fid: Fid, mode: u8) -> Result<Qid> {
        let op = self.protocol.open(fid, mode).map_err(protocol_error)?;
        match self.call_op(op)? {
            Completion::Open { qid, .. } => Ok(qid),
            other => Err(unexpected("Ropen", other)),
        }
    }

    pub fn create(
        &mut self,
        parent_fid: Fid,
        name: &[u8],
        perm: u32,
        mode: u8,
    ) -> Result<(Fid, Qid)> {
        let fid = self.clone_fid(parent_fid)?;
        let op = self
            .protocol
            .create(fid, name.to_vec(), perm, mode)
            .map_err(protocol_error)?;
        let reply = self.call_op(op);
        match reply {
            Ok(Completion::Create { qid, .. }) => Ok((fid, qid)),
            Ok(other) => {
                let _ = self.clunk(fid);
                Err(unexpected("Rcreate", other))
            }
            Err(error) => {
                let _ = self.clunk(fid);
                Err(error)
            }
        }
    }

    pub fn read(&mut self, fid: Fid, offset: u64, count: u32) -> Result<Vec<u8>> {
        let count = codec::clamp_read_count(self.protocol.msize(), count);
        let op = self
            .protocol
            .read(fid, offset, count)
            .map_err(protocol_error)?;
        match self.call_op(op)? {
            Completion::Read { data } => Ok(data),
            other => Err(unexpected("Rread", other)),
        }
    }

    pub fn write(&mut self, fid: Fid, mut offset: u64, mut data: &[u8]) -> Result<u32> {
        if data.is_empty() {
            return self.write_once(fid, offset, data);
        }

        let mut total = 0_u32;
        let max = usize::try_from(self.max_write_payload()).unwrap_or(usize::MAX);
        while !data.is_empty() {
            let chunk_len = data.len().min(max);
            let chunk = &data[..chunk_len];
            let count = self.write_once(fid, offset, chunk)?;
            if count == 0 {
                return Err(Error::from("zero-length 9P write progress"));
            }
            let count_usize =
                usize::try_from(count).map_err(|_| Error::from("write count overflow"))?;
            if count_usize > chunk_len {
                return Err(Error::from(
                    "9P server reported more bytes written than requested",
                ));
            }
            total = total.saturating_add(count);
            offset = offset.saturating_add(u64::from(count));
            data = &data[count_usize..];
            if count_usize < chunk_len {
                break;
            }
        }
        Ok(total)
    }

    pub fn write_once(&mut self, fid: Fid, offset: u64, data: &[u8]) -> Result<u32> {
        let op = self
            .protocol
            .write(fid, offset, data.to_vec())
            .map_err(protocol_error)?;
        match self.call_op(op)? {
            Completion::Write { count } => Ok(count),
            other => Err(unexpected("Rwrite", other)),
        }
    }

    pub fn clunk(&mut self, fid: Fid) -> Result<()> {
        let op = self.protocol.clunk(fid).map_err(protocol_error)?;
        match self.call_op(op)? {
            Completion::Clunk => Ok(()),
            other => Err(unexpected("Rclunk", other)),
        }
    }

    pub fn remove(&mut self, fid: Fid) -> Result<()> {
        let op = self.protocol.remove(fid).map_err(protocol_error)?;
        match self.call_op(op)? {
            Completion::Remove => Ok(()),
            other => Err(unexpected("Rremove", other)),
        }
    }

    pub fn stat(&mut self, fid: Fid) -> Result<Stat> {
        let op = self.protocol.stat(fid).map_err(protocol_error)?;
        match self.call_op(op)? {
            Completion::Stat { stat } => Ok(stat),
            other => Err(unexpected("Rstat", other)),
        }
    }

    pub fn wstat(&mut self, fid: Fid, stat: Stat) -> Result<()> {
        let op = self.protocol.wstat(fid, stat).map_err(protocol_error)?;
        match self.call_op(op)? {
            Completion::Wstat => Ok(()),
            other => Err(unexpected("Rwstat", other)),
        }
    }

    pub fn stat_path(&mut self, path: &str) -> Result<Stat> {
        let fid = self.walk_path(path)?;
        let result = self.stat(fid);
        let _ = self.clunk(fid);
        result
    }

    pub fn list_path(&mut self, path: &str) -> Result<Vec<Stat>> {
        let fid = self.walk_path(path)?;
        let stat = self.stat(fid)?;
        let result = if stat.mode & DMDIR != 0 {
            self.open(fid, OREAD)?;
            self.read_dir_stats(fid)
        } else {
            Err(Error::from(ENOTDIR))
        };
        let _ = self.clunk(fid);
        result
    }

    pub fn read_path(&mut self, path: &str) -> Result<Vec<u8>> {
        let fid = self.walk_path(path)?;
        let open = self.open(fid, OREAD);
        let result = match open {
            Ok(_) => self.read_all(fid),
            Err(error) => Err(error),
        };
        let _ = self.clunk(fid);
        result
    }

    pub fn read_path_range(&mut self, path: &str, offset: u64, count: u32) -> Result<Vec<u8>> {
        let fid = self.walk_path(path)?;
        let open = self.open(fid, OREAD);
        let result = match open {
            Ok(_) => self.read(fid, offset, count),
            Err(error) => Err(error),
        };
        let _ = self.clunk(fid);
        result
    }

    pub fn write_path(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<u32> {
        let fid = self.walk_path(path)?;
        let open = self.open(fid, OWRITE);
        let result = match open {
            Ok(_) => self.write(fid, offset, data),
            Err(error) => Err(error),
        };
        match result {
            Ok(count) => {
                self.clunk(fid)?;
                Ok(count)
            }
            Err(error) => {
                let _ = self.clunk(fid);
                Err(error)
            }
        }
    }

    pub fn write_file(&mut self, path: &str, data: &[u8]) -> Result<u32> {
        let fid = self.walk_path(path)?;
        let open = self.open(fid, OWRITE | OTRUNC);
        let result = match open {
            Ok(_) => self.write(fid, 0, data),
            Err(error) => Err(error),
        };
        match result {
            Ok(count) => {
                self.clunk(fid)?;
                Ok(count)
            }
            Err(error) => {
                let _ = self.clunk(fid);
                Err(error)
            }
        }
    }

    pub fn create_at(&mut self, parent: &str, name: &str, perm: u32, mode: u8) -> Result<Qid> {
        let parent_fid = self.walk_path(parent)?;
        let result = self.create(parent_fid, name.as_bytes(), perm, mode);
        let result = match result {
            Ok((fid, qid)) => match self.clunk(fid) {
                Ok(()) => Ok(qid),
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        match result {
            Ok(qid) => {
                self.clunk(parent_fid)?;
                Ok(qid)
            }
            Err(error) => {
                let _ = self.clunk(parent_fid);
                Err(error)
            }
        }
    }

    pub fn create_write_at(
        &mut self,
        parent: &str,
        name: &str,
        perm: u32,
        mode: u8,
        offset: u64,
        data: &[u8],
    ) -> Result<u32> {
        let parent_fid = self.walk_path(parent)?;
        let created = self.create(parent_fid, name.as_bytes(), perm, mode);
        let parent_clunk = self.clunk(parent_fid);
        let (fid, _) = created?;
        if let Err(error) = parent_clunk {
            let _ = self.clunk(fid);
            return Err(error);
        }

        let written = self.write(fid, offset, data);
        let clunked = self.clunk(fid);
        match written {
            Ok(count) => {
                clunked?;
                Ok(count)
            }
            Err(error) => Err(error),
        }
    }

    pub fn remove_path(&mut self, path: &str) -> Result<()> {
        let fid = self.walk_path(path)?;
        self.remove(fid)
    }

    pub fn rpc_path(&mut self, path: &str, data: &[u8]) -> Result<Vec<u8>> {
        let fid = self.walk_path(path)?;
        let open = self.open(fid, ORDWR);
        let result = match open {
            Ok(_) => {
                let count = self.write(fid, 0, data)?;
                if usize::try_from(count).unwrap_or(usize::MAX) != data.len() {
                    Err(Error::from("short rpc request write"))
                } else {
                    self.read_all(fid)
                }
            }
            Err(error) => Err(error),
        };
        let _ = self.clunk(fid);
        result
    }

    pub fn read_all(&mut self, fid: Fid) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut offset = 0_u64;
        loop {
            let data = self.read(fid, offset, DEFAULT_READ_CHUNK)?;
            if data.is_empty() {
                break;
            }
            offset = offset.saturating_add(
                u64::try_from(data.len()).map_err(|_| Error::from("read count overflow"))?,
            );
            out.extend(data);
        }
        Ok(out)
    }

    pub fn read_dir_stats(&mut self, fid: Fid) -> Result<Vec<Stat>> {
        let data = self.read_all(fid)?;
        decode_dir_entries(&data)
    }

    fn call_op(&mut self, op: Op) -> Result<Completion> {
        let expected_tag = op.tag;
        match self.call_message(op.message)? {
            ClientResponse::Completion { tag, completion } if tag == expected_tag => Ok(completion),
            ClientResponse::Error { tag, ename } if tag == expected_tag => Err(Error::new(ename)),
            other => Err(Error::from(format!(
                "response tag mismatch or unexpected response: {other:?}"
            ))),
        }
    }

    fn call_message(&mut self, message: TMessage) -> Result<ClientResponse> {
        codec::write_tmessage(&mut self.stream, &message)?;
        let response = self.read_response()?;
        self.protocol.receive(response).map_err(protocol_error)
    }

    fn read_response(&mut self) -> Result<crate::message::RMessage> {
        codec::read_rmessage(&mut self.stream)?
            .ok_or_else(|| Error::from("9P transport closed before response"))
    }
}

pub fn parse_tcp_address(address: &str) -> Result<String> {
    if let Some(rest) = address.strip_prefix("tcp!") {
        let parts = rest.split('!').collect::<Vec<_>>();
        if parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty() {
            return Ok(format!("{}:{}", parts[0], parts[1]));
        }
        return Err(Error::from(format!("invalid tcp address {address}")));
    }
    if address.contains('!') {
        return Err(Error::from(format!("unsupported 9P address {address}")));
    }
    if address.contains(':') {
        return Ok(address.to_string());
    }
    Ok(format!("{address}:564"))
}

fn connect_tcp_stream_with_timeouts(
    socket: &str,
    timeouts: TcpConnectionTimeouts,
) -> Result<TcpStream> {
    let timeouts = timeouts.validate()?;
    let addresses = socket
        .to_socket_addrs()
        .map_err(|error| io_error(format!("resolve {socket}"), error))?;
    let mut last_error = None;
    for address in addresses {
        match TcpStream::connect_timeout(&address, timeouts.connect_timeout) {
            Ok(stream) => {
                stream
                    .set_nodelay(true)
                    .map_err(|error| io_error("set TCP_NODELAY", error))?;
                stream
                    .set_read_timeout(Some(timeouts.read_timeout))
                    .map_err(|error| io_error("set TCP read timeout", error))?;
                stream
                    .set_write_timeout(Some(timeouts.write_timeout))
                    .map_err(|error| io_error("set TCP write timeout", error))?;
                return Ok(stream);
            }
            Err(error) => last_error = Some((address, error)),
        }
    }
    match last_error {
        Some((address, error)) => Err(io_error(format!("connect {socket} via {address}"), error)),
        None => Err(Error::from(format!(
            "connect {socket}: no socket addresses resolved"
        ))),
    }
}

fn validate_nonzero_timeout(label: &str, timeout: Duration) -> Result<()> {
    if timeout.is_zero() {
        return Err(Error::from(format!(
            "{label} timeout must be greater than zero"
        )));
    }
    Ok(())
}

pub fn path_names(path: &str) -> Vec<Vec<u8>> {
    path.split('/')
        .filter(|name| !name.is_empty() && *name != ".")
        .map(|name| name.as_bytes().to_vec())
        .collect()
}

fn op_fid(op: &Op) -> Result<Fid> {
    op.fid
        .ok_or_else(|| Error::from("9P operation did not allocate a fid"))
}

fn protocol_error(error: Error) -> Error {
    Error::from(format!("9P client state: {error}"))
}

fn io_error(context: impl AsRef<str>, error: std::io::Error) -> Error {
    if matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    ) {
        return Error::from(format!(
            "{}: 9P transport timeout or would-block: {error}",
            context.as_ref()
        ));
    }
    Error::from(format!("{}: {error}", context.as_ref()))
}

fn unexpected(expected: &str, got: impl std::fmt::Debug) -> Error {
    Error::from(format!("expected {expected}, got {got:?}"))
}

#[cfg(test)]
mod tests;
