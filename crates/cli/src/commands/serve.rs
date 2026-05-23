use std::{
    fs as std_fs,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, ToSocketAddrs},
    os::unix::fs::FileTypeExt,
    os::unix::net::UnixListener,
    path::{Path, PathBuf},
    thread,
};

use crate::export_descriptor::{
    AuthBoundary, AuthClass, ExportDescriptor, ExportMode, Protocol, TransportClass,
    EXPORT_FORMAT_V1,
};
use fs::LocalTree;
use r9p::{
    codec,
    message::TMessage,
    server::{Server, ServerConfig},
};

use crate::{
    errors::{cli_error, CliResult},
    target::Config,
};

const DEFAULT_MAX_FIDS: usize = 4096;
const FD_LIMIT_MARGIN: u64 = 256;

pub(crate) fn serve_cmd(global: Config, args: Vec<String>) -> CliResult<()> {
    let config = parse_serve_config(global, args)?;
    ensure_fd_budget(config.max_fids)?;
    let bound = BoundListener::bind(&config)?;
    eprintln!(
        "r9p: serving {} on {}",
        config.root.display(),
        bound.display_endpoint()
    );
    bound.run(config)
}

pub(crate) fn export_cmd(global: Config, args: Vec<String>) -> CliResult<()> {
    let config = parse_export_config(global, args)?;
    ensure_fd_budget(config.serve.max_fids)?;
    let bound = BoundListener::bind(&config.serve)?;
    let descriptor = export_descriptor(&config, &bound)?;
    write_descriptor(&config, &descriptor)?;
    bound.run(config.serve)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServeConfig {
    root: PathBuf,
    bind: BindTarget,
    uname: String,
    aname: String,
    msize: u32,
    max_fids: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BindTarget {
    Tcp(SocketAddr),
    Unix(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExportConfig {
    serve: ServeConfig,
    descriptor_file: Option<PathBuf>,
    auth: AuthBoundary,
}

enum BoundListener {
    Tcp(TcpListener),
    Unix {
        path: PathBuf,
        listener: UnixListener,
    },
}

pub(crate) fn parse_serve_config(global: Config, args: Vec<String>) -> CliResult<ServeConfig> {
    if global.address.is_some() {
        return Err(cli_error(
            "r9p serve uses --bind for its listen address; do not use global -a",
        ));
    }

    let mut bind = None;
    let mut max_fids = DEFAULT_MAX_FIDS;
    let mut positional = Vec::new();
    let mut index = 0_usize;
    while index < args.len() {
        match args[index].as_str() {
            "--bind" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| cli_error("missing bind address"))?;
                bind = Some(parse_bind_target(value)?);
            }
            "--max-fids" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| cli_error("missing max fid count"))?;
                max_fids = value
                    .parse::<usize>()
                    .map_err(|_| cli_error(format!("invalid max fid count {value}")))?;
            }
            "-h" | "--help" => serve_usage(0),
            arg if arg.starts_with('-') => {
                return Err(cli_error(format!("unknown serve option {arg}")));
            }
            arg => positional.push(arg.to_string()),
        }
        index += 1;
    }

    if positional.len() != 1 {
        return Err(cli_error("expected root directory"));
    }

    let bind = bind.unwrap_or_else(default_unix_bind);
    validate_serve_bind(&bind)?;
    Ok(ServeConfig {
        root: PathBuf::from(&positional[0]),
        bind,
        uname: global.uname,
        aname: global.aname,
        msize: global.msize,
        max_fids,
    })
}

fn parse_export_config(global: Config, args: Vec<String>) -> CliResult<ExportConfig> {
    if global.address.is_some() {
        return Err(cli_error(
            "r9p export uses --bind for its listen address; do not use global -a",
        ));
    }

    let mut bind = None;
    let mut max_fids = DEFAULT_MAX_FIDS;
    let mut descriptor_file = None;
    let mut descriptor_format = "machine".to_string();
    let mut auth = AuthBoundary::none();
    let mut positional = Vec::new();
    let mut index = 0_usize;
    while index < args.len() {
        match args[index].as_str() {
            "--bind" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| cli_error("missing bind address"))?;
                bind = Some(parse_bind_target(value)?);
            }
            "--max-fids" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| cli_error("missing max fid count"))?;
                max_fids = value
                    .parse::<usize>()
                    .map_err(|_| cli_error(format!("invalid max fid count {value}")))?;
            }
            "--descriptor" => {
                index += 1;
                descriptor_format = args
                    .get(index)
                    .ok_or_else(|| cli_error("missing descriptor format"))?
                    .clone();
            }
            "--descriptor-file" => {
                index += 1;
                descriptor_file = Some(PathBuf::from(
                    args.get(index)
                        .ok_or_else(|| cli_error("missing descriptor file"))?,
                ));
            }
            "--auth" => {
                index += 1;
                auth = AuthBoundary::parse(
                    args.get(index)
                        .ok_or_else(|| cli_error("missing auth boundary"))?,
                )?;
            }
            "-h" | "--help" => export_usage(0),
            arg if arg.starts_with('-') => {
                return Err(cli_error(format!("unknown export option {arg}")));
            }
            arg => positional.push(arg.to_string()),
        }
        index += 1;
    }

    if positional.len() != 1 {
        return Err(cli_error("expected root directory"));
    }
    if !matches!(descriptor_format.as_str(), "machine" | EXPORT_FORMAT_V1) {
        return Err(cli_error(format!(
            "unsupported descriptor format {descriptor_format}"
        )));
    }

    let bind = bind.unwrap_or_else(default_unix_bind);
    validate_export_bind(&bind, &auth)?;

    Ok(ExportConfig {
        serve: ServeConfig {
            root: PathBuf::from(&positional[0]),
            bind,
            uname: global.uname,
            aname: global.aname,
            msize: global.msize,
            max_fids,
        },
        descriptor_file,
        auth,
    })
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

fn required_nofile_limit(max_fids: usize) -> CliResult<libc::rlim_t> {
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
    let tree = LocalTree::open(&config.root)
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

fn parse_bind_target(value: &str) -> CliResult<BindTarget> {
    if let Some(path) = value.strip_prefix("unix:") {
        return parse_unix_bind(path);
    }
    if value.starts_with('/') {
        return parse_unix_bind(value);
    }
    if let Some(rest) = value.strip_prefix("tcp!") {
        let parts = rest.split('!').collect::<Vec<_>>();
        if parts.len() != 2 {
            return Err(cli_error(format!("invalid tcp bind address {value}")));
        }
        return parse_tcp_bind(&format!("{}:{}", parts[0], parts[1]));
    }
    parse_tcp_bind(value)
}

fn parse_unix_bind(path: &str) -> CliResult<BindTarget> {
    if path.is_empty() {
        return Err(cli_error("unix bind address requires a path"));
    }
    Ok(BindTarget::Unix(PathBuf::from(path)))
}

fn parse_tcp_bind(value: &str) -> CliResult<BindTarget> {
    let mut addrs = value
        .to_socket_addrs()
        .map_err(|error| cli_error(format!("invalid tcp bind address {value}: {error}")))?;
    let address = addrs
        .next()
        .ok_or_else(|| cli_error(format!("tcp bind address {value} resolved no addresses")))?;
    Ok(BindTarget::Tcp(address))
}

fn default_unix_bind() -> BindTarget {
    BindTarget::Unix(std::env::temp_dir().join(format!("r9p-serve-{}.sock", std::process::id())))
}

fn validate_serve_bind(bind: &BindTarget) -> CliResult<()> {
    if let BindTarget::Tcp(address) = bind {
        if !address.ip().is_loopback() {
            return Err(cli_error(
                "r9p serve only admits loopback TCP binds in Plan 47; use r9p export --auth for an authenticated network boundary",
            ));
        }
    }
    Ok(())
}

fn validate_export_bind(bind: &BindTarget, auth: &AuthBoundary) -> CliResult<()> {
    match bind {
        BindTarget::Tcp(address) if address.ip().is_loopback() => Ok(()),
        BindTarget::Tcp(_) => match auth.class {
            AuthClass::WireGuard | AuthClass::Tailscale => Ok(()),
            AuthClass::None => Err(cli_error(
                "r9p export requires --auth for non-loopback TCP binds",
            )),
            AuthClass::UnixPeerCred => Err(cli_error(
                "r9p export cannot use uds-peercred auth for TCP binds",
            )),
        },
        BindTarget::Unix(_) => match auth.class {
            AuthClass::None | AuthClass::UnixPeerCred => Ok(()),
            AuthClass::WireGuard | AuthClass::Tailscale => Err(cli_error(
                "r9p export cannot use network auth boundaries for unix socket binds",
            )),
        },
    }
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
        mode: ExportMode::ReadOnly,
        auth: config.auth.clone(),
        pid: std::process::id(),
        protocol: Protocol::NineP2000,
        msize: config.serve.msize,
        expires_at: None,
        local_root_label: Some(config.serve.root.display().to_string()),
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

fn serve_usage(code: i32) -> ! {
    eprintln!("usage: r9p serve [--bind address] [--max-fids count] root");
    std::process::exit(code);
}

fn export_usage(code: i32) -> ! {
    eprintln!(
        "usage: r9p export [--bind address] [--max-fids count] [--descriptor machine] [--descriptor-file path] [--auth boundary] root"
    );
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::{parse_export_config, parse_serve_config, required_nofile_limit, BindTarget};
    use crate::{target::Config, DEFAULT_MSIZE};
    use std::{net::SocketAddr, path::PathBuf};

    fn global() -> Config {
        Config {
            address: None,
            aname: String::new(),
            uname: "codex".to_string(),
            msize: DEFAULT_MSIZE,
            msize_set: false,
            machine: false,
        }
    }

    #[test]
    fn parses_loopback_tcp_bind() {
        let config = parse_serve_config(
            global(),
            vec![
                "--bind".to_string(),
                "127.0.0.1:0".to_string(),
                "/tmp/export".to_string(),
            ],
        )
        .expect("serve config should parse");
        assert_eq!(
            config.bind,
            BindTarget::Tcp("127.0.0.1:0".parse::<SocketAddr>().expect("socket address"))
        );
        assert_eq!(config.root, PathBuf::from("/tmp/export"));
    }

    #[test]
    fn nofile_budget_includes_fid_margin() {
        assert_eq!(4352, required_nofile_limit(4096).expect("limit"));
    }

    #[test]
    fn parses_plan9_tcp_bind() {
        let config = parse_serve_config(
            global(),
            vec![
                "--bind".to_string(),
                "tcp!127.0.0.1!0".to_string(),
                "/tmp/export".to_string(),
            ],
        )
        .expect("serve config should parse");
        assert_eq!(
            config.bind,
            BindTarget::Tcp("127.0.0.1:0".parse::<SocketAddr>().expect("socket address"))
        );
    }

    #[test]
    fn parses_unix_bind() {
        let config = parse_serve_config(
            global(),
            vec![
                "--bind".to_string(),
                "unix:/tmp/r9p.sock".to_string(),
                "/tmp/export".to_string(),
            ],
        )
        .expect("serve config should parse");
        assert_eq!(
            config.bind,
            BindTarget::Unix(PathBuf::from("/tmp/r9p.sock"))
        );
    }

    #[test]
    fn rejects_non_loopback_tcp_bind_without_auth_boundary() {
        let result = parse_serve_config(
            global(),
            vec![
                "--bind".to_string(),
                "192.0.2.10:564".to_string(),
                "/tmp/export".to_string(),
            ],
        );
        assert!(result.is_err());
    }

    #[test]
    fn parses_non_loopback_export_bind_with_network_auth_boundary() {
        let config = parse_export_config(
            global(),
            vec![
                "--bind".to_string(),
                "192.0.2.10:564".to_string(),
                "--auth".to_string(),
                "wg:m7-dev-lan".to_string(),
                "/tmp/export".to_string(),
            ],
        )
        .expect("export config should parse");
        assert_eq!(
            config.serve.bind,
            BindTarget::Tcp(
                "192.0.2.10:564"
                    .parse::<SocketAddr>()
                    .expect("socket address")
            )
        );
        assert_eq!(config.auth.render(), "wg:m7-dev-lan");
    }

    #[test]
    fn rejects_non_loopback_export_bind_without_auth_boundary() {
        let result = parse_export_config(
            global(),
            vec![
                "--bind".to_string(),
                "192.0.2.10:564".to_string(),
                "/tmp/export".to_string(),
            ],
        );
        assert!(result.is_err());
    }

    #[test]
    fn parses_export_descriptor_file_and_auth_boundary() {
        let config = parse_export_config(
            global(),
            vec![
                "--bind".to_string(),
                "unix:/tmp/r9p.sock".to_string(),
                "--descriptor-file".to_string(),
                "/tmp/r9p.desc".to_string(),
                "--auth".to_string(),
                "uds-peercred:1000:100".to_string(),
                "/tmp/export".to_string(),
            ],
        )
        .expect("export config should parse");
        assert_eq!(config.descriptor_file, Some(PathBuf::from("/tmp/r9p.desc")));
        assert_eq!(config.auth.render(), "uds-peercred:1000:100");
    }
}
