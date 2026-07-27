use snow::StatelessTransportState;
use std::{
    io::{self, Read, Write},
    net::TcpStream,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use r9p::multiplex::MultiplexTransport;

const AUTH_TAG_BYTES: usize = 16;
const MAX_CIPHERTEXT_BYTES: usize = u16::MAX as usize;
const MAX_PLAINTEXT_BYTES: usize = MAX_CIPHERTEXT_BYTES - AUTH_TAG_BYTES;

pub struct SecureStream<S = TcpStream> {
    transport_stream: S,
    transport: Arc<StatelessTransportState>,
    read_state: Arc<Mutex<ReadState>>,
    write_state: Arc<Mutex<WriteState>>,
}

#[derive(Default)]
struct ReadState {
    nonce: u64,
    plaintext: Vec<u8>,
    offset: usize,
}

#[derive(Default)]
struct WriteState {
    nonce: u64,
}

impl<S> SecureStream<S> {
    pub(crate) fn new(transport_stream: S, transport: StatelessTransportState) -> Self {
        Self {
            transport_stream,
            transport: Arc::new(transport),
            read_state: Arc::new(Mutex::new(ReadState::default())),
            write_state: Arc::new(Mutex::new(WriteState::default())),
        }
    }

    pub(crate) const fn transport_stream(&self) -> &S {
        &self.transport_stream
    }
}

impl<S: MultiplexTransport> SecureStream<S> {
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            transport_stream: self.transport_stream.try_clone_transport()?,
            transport: Arc::clone(&self.transport),
            read_state: Arc::clone(&self.read_state),
            write_state: Arc::clone(&self.write_state),
        })
    }

    pub fn shutdown(&self) -> io::Result<()> {
        self.transport_stream.shutdown_transport()
    }
}

impl SecureStream<TcpStream> {
    pub fn peer_closed(&self, timeout: Duration) -> io::Result<bool> {
        if timeout.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "peer-close observation timeout must be nonzero",
            ));
        }
        let previous = self.transport_stream.read_timeout()?;
        self.transport_stream.set_read_timeout(Some(timeout))?;
        let mut byte = [0_u8; 1];
        let observed = match self.transport_stream.peek(&mut byte) {
            Ok(0) => Ok(true),
            Ok(_) => Ok(false),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        };
        let restored = self.transport_stream.set_read_timeout(previous);
        match (observed, restored) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) | (_, Err(error)) => Err(error),
        }
    }
}

impl<S: MultiplexTransport> SecureStream<S> {
    fn read_record(&mut self, state: &mut ReadState) -> io::Result<bool> {
        let mut length = [0_u8; 2];
        match self.transport_stream.read(&mut length[..1])? {
            0 => return Ok(false),
            1 => {}
            _ => unreachable!("one-byte read returned more than one byte"),
        }
        self.transport_stream.read_exact(&mut length[1..])?;
        let ciphertext_len = usize::from(u16::from_be_bytes(length));
        if ciphertext_len < AUTH_TAG_BYTES {
            let _ = self.transport_stream.shutdown_transport();
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "encrypted 9P record is shorter than its authentication tag",
            ));
        }
        let mut ciphertext = vec![0_u8; ciphertext_len];
        self.transport_stream.read_exact(&mut ciphertext)?;
        let mut plaintext = vec![0_u8; ciphertext_len - AUTH_TAG_BYTES];
        let plaintext_len = self
            .transport
            .read_message(state.nonce, &ciphertext, &mut plaintext)
            .map_err(|error| {
                let _ = self.transport_stream.shutdown_transport();
                noise_io_error(error)
            })?;
        state.nonce = next_nonce(state.nonce).inspect_err(|_| {
            let _ = self.transport_stream.shutdown_transport();
        })?;
        plaintext.truncate(plaintext_len);
        state.plaintext = plaintext;
        state.offset = 0;
        Ok(true)
    }
}

impl<S: MultiplexTransport> Read for SecureStream<S> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let read_state = Arc::clone(&self.read_state);
        let mut state = lock(&read_state, "encrypted 9P read state")?;
        loop {
            if state.offset < state.plaintext.len() {
                let count = output
                    .len()
                    .min(state.plaintext.len().saturating_sub(state.offset));
                output[..count]
                    .copy_from_slice(&state.plaintext[state.offset..state.offset + count]);
                state.offset += count;
                if state.offset == state.plaintext.len() {
                    state.plaintext.clear();
                    state.offset = 0;
                }
                return Ok(count);
            }
            if !self.read_record(&mut state)? {
                return Ok(0);
            }
        }
    }
}

impl<S: MultiplexTransport> Write for SecureStream<S> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        if input.is_empty() {
            return Ok(0);
        }
        let count = input.len().min(MAX_PLAINTEXT_BYTES);
        let mut ciphertext = vec![0_u8; count + AUTH_TAG_BYTES];
        let write_state = Arc::clone(&self.write_state);
        let mut state = lock(&write_state, "encrypted 9P write state")?;
        let ciphertext_len = self
            .transport
            .write_message(state.nonce, &input[..count], &mut ciphertext)
            .map_err(|error| {
                let _ = self.transport_stream.shutdown_transport();
                noise_io_error(error)
            })?;
        let encoded_len = u16::try_from(ciphertext_len).map_err(|_| {
            let _ = self.transport_stream.shutdown_transport();
            io::Error::new(
                io::ErrorKind::InvalidData,
                "encrypted 9P record exceeds its framing limit",
            )
        })?;
        state.nonce = next_nonce(state.nonce).inspect_err(|_| {
            let _ = self.transport_stream.shutdown_transport();
        })?;
        if let Err(error) = self
            .transport_stream
            .write_all(&encoded_len.to_be_bytes())
            .and_then(|()| {
                self.transport_stream
                    .write_all(&ciphertext[..ciphertext_len])
            })
        {
            let _ = self.transport_stream.shutdown_transport();
            return Err(error);
        }
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        let write_state = Arc::clone(&self.write_state);
        let _state = lock(&write_state, "encrypted 9P write state")?;
        self.transport_stream.flush()
    }
}

impl<S: MultiplexTransport> r9p::server::ConnectionStream for SecureStream<S> {
    fn try_clone_stream(&self) -> io::Result<Self> {
        self.try_clone()
    }
}

impl<S: MultiplexTransport> r9p::multiplex::MultiplexTransport for SecureStream<S> {
    fn try_clone_transport(&self) -> io::Result<Self> {
        self.try_clone()
    }

    fn shutdown_transport(&self) -> io::Result<()> {
        self.shutdown()
    }
}

fn lock<'a, T>(mutex: &'a Mutex<T>, label: &str) -> io::Result<MutexGuard<'a, T>> {
    mutex
        .lock()
        .map_err(|_| io::Error::other(format!("{label} poisoned")))
}

fn next_nonce(nonce: u64) -> io::Result<u64> {
    nonce.checked_add(1).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "encrypted 9P session exhausted its record nonce",
        )
    })
}

fn noise_io_error(error: snow::Error) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("encrypted 9P record: {error}"),
    )
}
