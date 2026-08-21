use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    net::{TcpListener, TcpStream},
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

use r9p::{
    codec::Variant,
    server::{serve_connection, ServerConfig as R9pServerConfig},
};
use r9p_auth::{authenticate_server, ServerConfig as SessionAuthConfig};

use crate::errors::{cli_error, CliResult};

use super::{config::StreamExportConfig, handler::ProcessStream};

const AUTHENTICATION_TIMEOUT: Duration = Duration::from_secs(10);
const STREAM_MAX_FIDS: usize = 8;
const STREAM_MAX_ASYNC_REQUESTS: usize = 4;

pub(super) fn run(config: StreamExportConfig) -> CliResult<()> {
    let auth = SessionAuthConfig::read(&config.auth_config)?;
    let listener = TcpListener::bind(config.bind)
        .map_err(|error| cli_error(format!("bind {}: {error}", config.bind)))?;
    let bound = listener
        .local_addr()
        .map_err(|error| cli_error(format!("inspect stream-export listener: {error}")))?;
    let status = status(&config, bound.to_string(), &auth);
    write_status(config.status_file.as_deref(), status.as_bytes())?;
    eprintln!("r9p: exporting process stream on {bound}");

    let sessions = Arc::new(SessionCounter::new(config.max_sessions));
    loop {
        let (stream, _) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(error) => {
                eprintln!("r9p: accept stream-export connection: {error}");
                continue;
            }
        };
        let Some(session_slot) = sessions.acquire() else {
            eprintln!(
                "r9p: reject stream-export connection: maximum {} sessions are active",
                config.max_sessions
            );
            drop(stream);
            continue;
        };
        let auth = auth.clone();
        let allowed_principals = config.allowed_principals.clone();
        let command = config.command.clone();
        let max_buffer_bytes = config.max_buffer_bytes;
        let msize = config.msize;
        let spawn = thread::Builder::new()
            .name("r9p-stream-export-session".to_string())
            .spawn(move || {
                let _session_slot = session_slot;
                serve_authenticated_stream(
                    stream,
                    auth,
                    allowed_principals,
                    command,
                    max_buffer_bytes,
                    msize,
                );
            });
        if let Err(error) = spawn {
            eprintln!("r9p: start stream-export session: {error}");
        }
    }
}

fn serve_authenticated_stream(
    stream: TcpStream,
    auth: SessionAuthConfig,
    allowed_principals: BTreeSet<String>,
    command: super::config::ProcessCommand,
    max_buffer_bytes: usize,
    msize: u32,
) {
    let session = match authenticate_server(stream, &auth, AUTHENTICATION_TIMEOUT) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("r9p: reject unauthenticated stream-export connection: {error}");
            return;
        }
    };
    let principal = session.peer.principal().to_string();
    if !allowed_principals.contains(&principal) {
        eprintln!("r9p: reject unauthorized stream-export principal {principal}");
        return;
    }

    let handler = Arc::new(ProcessStream::new(command, max_buffer_bytes));
    if let Err(error) = serve_connection(
        session.stream,
        R9pServerConfig {
            default_msize: msize,
            max_msize: msize,
            max_fids: STREAM_MAX_FIDS,
            max_async_requests: STREAM_MAX_ASYNC_REQUESTS,
            variant: Variant::R,
            session_uname: Some(principal.into_bytes()),
            ..R9pServerConfig::default()
        },
        handler,
    ) {
        eprintln!("r9p: serve stream-export connection: {error}");
    }
}

fn status(config: &StreamExportConfig, endpoint_bind: String, auth: &SessionAuthConfig) -> String {
    format!(
        "kind\tr9p-stream-export\nendpoint_bind\t{endpoint_bind}\naname\t{}\nstream_path\t/stream\nauth\tp9any:noise-xx@{}\npid\t{}\nprotocol\t9P2000.R\nmsize\t{}\nmax_sessions\t{}\nmax_buffer_bytes\t{}\n",
        config.aname,
        auth.domain(),
        std::process::id(),
        config.msize,
        config.max_sessions,
        config.max_buffer_bytes,
    )
}

fn write_status(path: Option<&Path>, bytes: &[u8]) -> CliResult<()> {
    match path {
        Some(path) => write_status_file(path, bytes),
        None => {
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(bytes)?;
            stdout.flush()?;
            Ok(())
        }
    }
}

fn write_status_file(path: &Path, bytes: &[u8]) -> CliResult<()> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| cli_error(format!("invalid status path {}", path.display())))?;
    let temporary = path.with_file_name(format!(".{file_name}.{}.temporary", std::process::id()));
    fs::write(&temporary, bytes).map_err(|error| {
        cli_error(format!(
            "write stream-export status {}: {error}",
            temporary.display()
        ))
    })?;
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(cli_error(format!(
            "publish stream-export status {}: {error}",
            path.display()
        )));
    }
    Ok(())
}

struct SessionCounter {
    active: AtomicUsize,
    maximum: usize,
}

impl SessionCounter {
    const fn new(maximum: usize) -> Self {
        Self {
            active: AtomicUsize::new(0),
            maximum,
        }
    }

    fn acquire(self: &Arc<Self>) -> Option<SessionSlot> {
        let acquired = self
            .active
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |active| {
                (active < self.maximum).then_some(active + 1)
            })
            .is_ok();
        acquired.then(|| SessionSlot(Arc::clone(self)))
    }
}

struct SessionSlot(Arc<SessionCounter>);

impl Drop for SessionSlot {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}
