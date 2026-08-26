use super::{front_port, parse_u64, response_line, ClientDispatcher, PeerClientServer};
use std::{
    io::{self, BufRead, Write},
    sync::{
        mpsc::{sync_channel, Receiver, SyncSender},
        Arc, Mutex,
    },
    thread,
};

const ORDINARY_WORKERS: usize = 16;
const ORDINARY_QUEUE_CAPACITY: usize = 16;

struct OrdinaryRequest {
    request_id: u64,
    command: String,
}

struct OrdinaryPool {
    requests: SyncSender<OrdinaryRequest>,
}

impl OrdinaryPool {
    fn new(dispatcher: ClientDispatcher, stdout: Arc<Mutex<io::Stdout>>) -> Self {
        let (requests, receiver) = sync_channel(ORDINARY_QUEUE_CAPACITY);
        let receiver = Arc::new(Mutex::new(receiver));
        for _ in 0..ORDINARY_WORKERS {
            let dispatcher = dispatcher.clone();
            let receiver = receiver.clone();
            let stdout = stdout.clone();
            thread::spawn(move || ordinary_worker(dispatcher, receiver, stdout));
        }
        Self { requests }
    }

    fn submit(&self, request_id: u64, command: &str) -> Result<(), String> {
        self.requests
            .send(OrdinaryRequest {
                request_id,
                command: command.to_string(),
            })
            .map_err(|_| "r9p beam port ordinary queue closed".to_string())
    }
}

fn ordinary_worker(
    dispatcher: ClientDispatcher,
    requests: Arc<Mutex<Receiver<OrdinaryRequest>>>,
    stdout: Arc<Mutex<io::Stdout>>,
) {
    loop {
        let request = match requests.lock() {
            Ok(requests) => requests.recv(),
            Err(_) => return,
        };
        let Ok(request) = request else {
            return;
        };
        let _ = write_response(
            &stdout,
            request.request_id,
            dispatcher.dispatch_line(&request.command),
        );
    }
}

pub(super) enum ResponseWork {
    Ready(Result<String, String>),
    Pending(front_port::PendingRequest),
}

impl ResponseWork {
    #[cfg(test)]
    pub(super) fn complete(self) -> Result<String, String> {
        match self {
            Self::Ready(response) => response,
            Self::Pending(request) => request.complete(),
        }
    }
}

pub(super) fn run() -> Result<(), String> {
    let stdin = io::stdin();
    let mut server = PeerClientServer::default();
    let stdout = Arc::new(Mutex::new(io::stdout()));
    let ordinary = OrdinaryPool::new(server.client_dispatcher(), stdout.clone());

    for line in stdin.lock().lines() {
        let line = line.map_err(|error| format!("read r9p beam port stdin: {error}"))?;
        let (request_id, command) = request_line(&line)?;
        if !command.starts_with("front-") {
            ordinary.submit(request_id, command)?;
            continue;
        }
        let work = server.dispatch_line(command);
        match work {
            ResponseWork::Ready(response) => write_response(&stdout, request_id, response)?,
            ResponseWork::Pending(request) => {
                let stdout = stdout.clone();
                thread::spawn(move || {
                    let _ = write_response(&stdout, request_id, request.complete());
                });
            }
        }
    }

    Ok(())
}

fn request_line(line: &str) -> Result<(u64, &str), String> {
    let Some((request_id, command)) = line.split_once('\t') else {
        return Err("invalid_r9p_beam_port_request_envelope".to_string());
    };
    let request_id = parse_u64("request_id", request_id)?;
    if command.is_empty() {
        return Err("invalid_r9p_beam_port_request_envelope".to_string());
    }
    Ok((request_id, command))
}

fn write_response(
    stdout: &Mutex<io::Stdout>,
    request_id: u64,
    response: Result<String, String>,
) -> Result<(), String> {
    let mut stdout = stdout
        .lock()
        .map_err(|_| "r9p beam port stdout lock poisoned".to_string())?;
    writeln!(stdout, "{request_id}\t{}", response_line(response))
        .map_err(|error| format!("write r9p beam port stdout: {error}"))?;
    stdout
        .flush()
        .map_err(|error| format!("flush r9p beam port stdout: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_envelope_preserves_the_command() {
        assert_eq!(request_line("19\tfront-new"), Ok((19, "front-new")));
    }

    #[test]
    fn request_envelope_requires_an_id_and_command() {
        assert!(request_line("front-new").is_err());
        assert!(request_line("19\t").is_err());
    }
}
