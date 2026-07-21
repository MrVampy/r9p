use snow::StatelessTransportState;
use std::{
    io::{self, Read, Write},
    net::{Shutdown, TcpStream},
    sync::{Arc, Mutex, MutexGuard},
};

const AUTH_TAG_BYTES: usize = 16;
const MAX_CIPHERTEXT_BYTES: usize = u16::MAX as usize;
const MAX_PLAINTEXT_BYTES: usize = MAX_CIPHERTEXT_BYTES - AUTH_TAG_BYTES;

pub struct SecureStream {
    socket: TcpStream,
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

impl SecureStream {
    pub(crate) fn new(socket: TcpStream, transport: StatelessTransportState) -> Self {
        Self {
            socket,
            transport: Arc::new(transport),
            read_state: Arc::new(Mutex::new(ReadState::default())),
            write_state: Arc::new(Mutex::new(WriteState::default())),
        }
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            socket: self.socket.try_clone()?,
            transport: Arc::clone(&self.transport),
            read_state: Arc::clone(&self.read_state),
            write_state: Arc::clone(&self.write_state),
        })
    }

    pub fn shutdown(&self) -> io::Result<()> {
        self.socket.shutdown(Shutdown::Both)
    }

    fn read_record(&mut self, state: &mut ReadState) -> io::Result<bool> {
        let mut length = [0_u8; 2];
        match self.socket.read(&mut length[..1])? {
            0 => return Ok(false),
            1 => {}
            _ => unreachable!("one-byte read returned more than one byte"),
        }
        self.socket.read_exact(&mut length[1..])?;
        let ciphertext_len = usize::from(u16::from_be_bytes(length));
        if ciphertext_len < AUTH_TAG_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "encrypted 9P record is shorter than its authentication tag",
            ));
        }
        let mut ciphertext = vec![0_u8; ciphertext_len];
        self.socket.read_exact(&mut ciphertext)?;
        let mut plaintext = vec![0_u8; ciphertext_len - AUTH_TAG_BYTES];
        let plaintext_len = self
            .transport
            .read_message(state.nonce, &ciphertext, &mut plaintext)
            .map_err(noise_io_error)?;
        state.nonce = next_nonce(state.nonce)?;
        plaintext.truncate(plaintext_len);
        state.plaintext = plaintext;
        state.offset = 0;
        Ok(true)
    }
}

impl Read for SecureStream {
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

impl Write for SecureStream {
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
            .map_err(noise_io_error)?;
        let encoded_len = u16::try_from(ciphertext_len).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "encrypted 9P record exceeds its framing limit",
            )
        })?;
        self.socket.write_all(&encoded_len.to_be_bytes())?;
        self.socket.write_all(&ciphertext[..ciphertext_len])?;
        state.nonce = next_nonce(state.nonce)?;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        let write_state = Arc::clone(&self.write_state);
        let _state = lock(&write_state, "encrypted 9P write state")?;
        self.socket.flush()
    }
}

impl r9p::server::ConnectionStream for SecureStream {
    fn try_clone_stream(&self) -> io::Result<Self> {
        self.try_clone()
    }
}

impl r9p::multiplex::MultiplexTransport for SecureStream {
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
