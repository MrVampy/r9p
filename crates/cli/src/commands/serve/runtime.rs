use std::{
    fs as std_fs,
    io::Write,
    net::{TcpListener, TcpStream},
    os::unix::fs::FileTypeExt,
    os::unix::net::UnixListener,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use fs::{LocalTree, LocalTreeConfig};
use r9p::{
    codec::Variant,
    export_descriptor::{ExportDescriptor, ExportMode, Protocol, TransportClass},
    server::{
        serve_file_tree_connection as serve_protocol_file_tree, ConnectionStream, ServerConfig,
    },
};
use r9p_auth::{authenticate_server, ServerConfig as AuthConfig};

use crate::errors::{cli_error, CliResult};

use super::config::{BindTarget, ExportConfig, ServeConfig};

const FD_LIMIT_MARGIN: u64 = 256;
const AUTH_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) fn serve(config: ServeConfig) -> CliResult<()> {
    ensure_fd_budget(config.max_fids)?;
    let bound = BoundListener::bind(&config)?;
    eprintln!(
        "r9p: serving {} on {}",
        config.root.display(),
        bound.display_endpoint()
    );
    bound.run(config, None)
}

pub(super) fn export(config: ExportConfig) -> CliResult<()> {
    ensure_fd_budget(config.serve.max_fids)?;
    let bound = BoundListener::bind(&config.serve)?;
    let descriptor = export_descriptor(&config, &bound)?;
    write_descriptor(&config, &descriptor)?;
    bound.run(config.serve, config.auth_config)
}

enum BoundListener {
    Tcp(TcpListener),
    Unix {
        path: PathBuf,
        listener: UnixListener,
    },
}

impl BoundListener {
    fn bind(config: &ServeConfig) -> CliResult<Self> {
        match &config.bind {
            BindTarget::Tcp(address) => {
                let listener = TcpListener::bind(address)
                    .map_err(|error| cli_error(format!("bind {address}: {error}")))?;
                Ok(Self::Tcp(listener))
            }
            BindTarget::Unix(path) => {
                remove_stale_socket(path)?;
                let listener = UnixListener::bind(path)
                    .map_err(|error| cli_error(format!("bind {}: {error}", path.display())))?;
                Ok(Self::Unix {
                    path: path.clone(),
                    listener,
                })
            }
        }
    }

    fn endpoint_bind(&self) -> CliResult<String> {
        match self {
            Self::Tcp(listener) => Ok(listener.local_addr()?.to_string()),
            Self::Unix { path, .. } => Ok(format!("unix:{}", path.display())),
        }
    }

    fn display_endpoint(&self) -> String {
        self.endpoint_bind()
            .unwrap_or_else(|_| "<unavailable>".to_string())
    }

    const fn transport_class(&self) -> TransportClass {
        match self {
            Self::Tcp(_) => TransportClass::Tcp,
            Self::Unix { .. } => TransportClass::Unix,
        }
    }

    fn run(self, config: ServeConfig, auth_config: Option<PathBuf>) -> CliResult<()> {
        let auth = auth_config.as_deref().map(AuthConfig::read).transpose()?;
        match self {
            Self::Tcp(listener) => {
                for stream in listener.incoming() {
                    let stream = stream
                        .map_err(|error| cli_error(format!("accept TCP connection: {error}")))?;
                    match &auth {
                        Some(auth) => {
                            spawn_authenticated_connection(stream, config.clone(), auth.clone())
                        }
                        None => spawn_connection(stream, config.clone(), None),
                    }
                }
            }
            Self::Unix { listener, .. } => {
                for stream in listener.incoming() {
                    let stream = stream
                        .map_err(|error| cli_error(format!("accept unix connection: {error}")))?;
                    spawn_connection(stream, config.clone(), None);
                }
            }
        }
        Ok(())
    }
}

fn spawn_authenticated_connection(stream: TcpStream, config: ServeConfig, auth: AuthConfig) {
    thread::spawn(move || {
        let session = match authenticate_server(stream, &auth, AUTH_HANDSHAKE_TIMEOUT) {
            Ok(session) => session,
            Err(error) => {
                eprintln!("r9p: reject unauthenticated connection: {error}");
                return;
            }
        };
        let principal = session.peer.principal().as_bytes().to_vec();
        if let Err(error) = serve_connection(session.stream, config, Some(principal)) {
            eprintln!("r9p: serve connection: {error}");
        }
    });
}

fn spawn_connection<S>(stream: S, config: ServeConfig, session_uname: Option<Vec<u8>>)
where
    S: ConnectionStream,
{
    thread::spawn(move || {
        if let Err(error) = serve_connection(stream, config, session_uname) {
            eprintln!("r9p: serve connection: {error}");
        }
    });
}

fn ensure_fd_budget(max_fids: usize) -> CliResult<()> {
    let target = required_nofile_limit(max_fids)?;
    let mut current = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    let status = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut current) };
    if status != 0 {
        return Err(cli_error(format!(
            "getrlimit RLIMIT_NOFILE: {}",
            std::io::Error::last_os_error()
        )));
    }
    if current.rlim_cur >= target {
        return Ok(());
    }
    if current.rlim_max < target {
        return Err(cli_error(format!(
            "r9p serve/export requires RLIMIT_NOFILE >= {target} for --max-fids {max_fids}, hard limit is {}",
            current.rlim_max
        )));
    }
    let desired = libc::rlimit {
        rlim_cur: target,
        rlim_max: current.rlim_max,
    };
    let status = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &desired) };
    if status == 0 {
        Ok(())
    } else {
        Err(cli_error(format!(
            "setrlimit RLIMIT_NOFILE to {target}: {}",
            std::io::Error::last_os_error()
        )))
    }
}

pub(super) fn required_nofile_limit(max_fids: usize) -> CliResult<libc::rlim_t> {
    let max_fids = libc::rlim_t::try_from(max_fids)
        .map_err(|_| cli_error(format!("max fid count too large {max_fids}")))?;
    Ok(max_fids.saturating_add(FD_LIMIT_MARGIN))
}

fn remove_stale_socket(path: &Path) -> CliResult<()> {
    let metadata = match std_fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(cli_error(format!(
                "stat bind path {}: {error}",
                path.display()
            )));
        }
    };
    if !metadata.file_type().is_socket() {
        return Err(cli_error(format!(
            "bind path {} already exists and is not a socket",
            path.display()
        )));
    }
    std_fs::remove_file(path)
        .map_err(|error| cli_error(format!("remove stale socket {}: {error}", path.display())))
}

fn serve_connection<S>(
    stream: S,
    config: ServeConfig,
    session_uname: Option<Vec<u8>>,
) -> CliResult<()>
where
    S: ConnectionStream,
{
    let tree = LocalTree::open_with_config(
        &config.root,
        LocalTreeConfig {
            writable: config.writable,
        },
    )
    .map_err(|error| cli_error(format!("open export root: {error}")))?;
    serve_protocol_file_tree(
        stream,
        ServerConfig {
            default_msize: config.msize,
            max_msize: config.msize,
            max_fids: config.max_fids,
            variant: Variant::R,
            session_uname,
            ..ServerConfig::default()
        },
        tree,
    )
    .map_err(|error| cli_error(format!("serve 9P connection: {error}")))
}

fn export_descriptor(config: &ExportConfig, bound: &BoundListener) -> CliResult<ExportDescriptor> {
    let aname = if config.serve.aname.is_empty() {
        "/".to_string()
    } else {
        config.serve.aname.clone()
    };
    Ok(ExportDescriptor {
        endpoint_bind: bound.endpoint_bind()?,
        aname: aname.clone(),
        uname: config.serve.uname.clone(),
        exported_root: aname,
        transport_class: bound.transport_class(),
        mode: if config.serve.writable {
            ExportMode::ReadWrite
        } else {
            ExportMode::ReadOnly
        },
        auth: config.auth.clone(),
        pid: std::process::id(),
        protocol: Protocol::NineP2000R,
        msize: config.serve.msize,
        expires_at: None,
        local_root_label: Some(config.serve.root.display().to_string()),
        namespace_mount_paths: Vec::new(),
        extra_fields: config.extra_fields.clone(),
    })
}

fn write_descriptor(config: &ExportConfig, descriptor: &ExportDescriptor) -> CliResult<()> {
    let rendered = descriptor.render()?;
    let _validated = ExportDescriptor::parse(&rendered)?;
    if let Some(path) = &config.descriptor_file {
        std_fs::write(path, rendered)
            .map_err(|error| cli_error(format!("write descriptor {}: {error}", path.display())))?;
    } else {
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(rendered.as_bytes())?;
        stdout.flush()?;
    }
    Ok(())
}
