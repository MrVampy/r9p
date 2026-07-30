use super::{front_port, parse_u64, response_line, PeerClientServer};
use std::{
    io::{self, BufRead, Write},
    sync::{Arc, Mutex},
    thread,
};

pub(super) enum ResponseWork {
    Ready(Result<String, String>),
    Pending(front_port::PendingRequest),
}

impl ResponseWork {
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

    for line in stdin.lock().lines() {
        let line = line.map_err(|error| format!("read r9p beam port stdin: {error}"))?;
        let (request_id, command) = request_line(&line)?;
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
