use std::{
    fs as std_fs,
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::FileTypeExt,
    os::unix::net::UnixListener,
    path::{Path, PathBuf},
    thread,
};

use fs::{LocalTree, LocalTreeConfig};
use r9p::{
    codec,
    export_descriptor::{ExportDescriptor, ExportMode, Protocol, TransportClass},
    message::TMessage,
    server::{Server, ServerConfig},
};

use crate::errors::{cli_error, CliResult};

use super::config::{BindTarget, ExportConfig, ServeConfig};

const FD_LIMIT_MARGIN: u64 = 256;

pub(super) fn serve(config: ServeConfig) -> CliResult<()> {
    ensure_fd_budget(config.max_fids)?;
    let bound = BoundListener::bind(&config)?;
    eprintln!(
        "r9p: serving {} on {}",
        config.root.display(),
        bound.display_endpoint()
    );
    bound.run(config)
}

pub(super) fn export(config: ExportConfig) -> CliResult<()> {
    ensure_fd_budget(config.serve.max_fids)?;
    let bound = BoundListener::bind(&config.serve)?;
    let descriptor = export_descriptor(&config, &bound)?;
    write_descriptor(&config, &descriptor)?;
    bound.run(config.serve)
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

    fn run(self, config: ServeConfig) -> CliResult<()> {
        match self {
            Self::Tcp(listener) => {
                for stream in listener.incoming() {
                    let stream = stream
                        .map_err(|error| cli_error(format!("accept TCP connection: {error}")))?;
                    spawn_connection(stream, config.clone());
                }
            }
            Self::Unix { listener, .. } => {
                for stream in listener.incoming() {
                    let stream = stream
                        .map_err(|error| cli_error(format!("accept unix connection: {error}")))?;
                    spawn_connection(stream, config.clone());
                }
            }
        }
        Ok(())
    }
}

fn spawn_connection<S>(stream: S, config: ServeConfig)
where
    S: Read + Write + Send + 'static,
{
    thread::spawn(move || {
        if let Err(error) = serve_connection(stream, config) {
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

fn serve_connection<S>(mut stream: S, config: ServeConfig) -> CliResult<()>
where
    S: Read + Write,
{
    let tree = LocalTree::open_with_config(
        &config.root,
        LocalTreeConfig {
            writable: config.writable,
        },
    )
    .map_err(|error| cli_error(format!("open export root: {error}")))?;
    let mut server = Server::with_config(
        tree,
        ServerConfig {
            default_msize: config.msize,
            max_msize: config.msize,
            max_fids: config.max_fids,
            ..ServerConfig::default()
        },
    );

    loop {
        let message = match read_tmessage(&mut stream) {
            Ok(message) => message,
            Err(error) if is_eof_error(error.as_ref()) => return Ok(()),
            Err(error) => return Err(error),
        };
        let reply = server.handle(message);
        let frame = codec::encode_rmessage_checked(&reply, server.session().msize())
            .map_err(|error| cli_error(format!("encode 9P reply: {error}")))?;
        stream
            .write_all(&frame)
            .map_err(|error| cli_error(format!("write 9P reply: {error}")))?;
        stream
            .flush()
            .map_err(|error| cli_error(format!("flush 9P reply: {error}")))?;
    }
}

fn read_tmessage(stream: &mut impl Read) -> CliResult<TMessage> {
    let mut prefix = [0_u8; 4];
    stream.read_exact(&mut prefix).map_err(|error| {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            Box::new(error) as Box<dyn std::error::Error>
        } else {
            cli_error(format!("read 9P frame size: {error}"))
        }
    })?;
    let size = u32::from_le_bytes(prefix);
    if size < codec::FRAME_HEADER_SIZE {
        return Err(cli_error(format!("short 9P frame {size}")));
    }
    let rest_len = usize::try_from(size - 4)?;
    let mut frame = Vec::with_capacity(rest_len + 4);
    frame.extend(prefix);
    frame.resize(rest_len + 4, 0);
    stream
        .read_exact(&mut frame[4..])
        .map_err(|error| cli_error(format!("read 9P frame body: {error}")))?;
    Ok(codec::decode_tmessage(&frame)?)
}

fn is_eof_error(error: &(dyn std::error::Error + 'static)) -> bool {
    let Some(io_error) = error.downcast_ref::<std::io::Error>() else {
        return false;
    };
    io_error.kind() == std::io::ErrorKind::UnexpectedEof
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
        protocol: Protocol::NineP2000,
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
