use std::{
    fs as std_fs,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, ToSocketAddrs},
    os::unix::fs::FileTypeExt,
    os::unix::net::UnixListener,
    path::{Path, PathBuf},
    thread,
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

pub(crate) fn serve_cmd(global: Config, args: Vec<String>) -> CliResult<()> {
    let config = parse_serve_config(global, args)?;
    serve(config)
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

pub(crate) fn parse_serve_config(global: Config, args: Vec<String>) -> CliResult<ServeConfig> {
    if global.address.is_some() {
        return Err(cli_error(
            "r9p serve uses --bind for its listen address; do not use global -a",
        ));
    }

    let mut bind = None;
    let mut max_fids = 4096_usize;
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
    Ok(ServeConfig {
        root: PathBuf::from(&positional[0]),
        bind,
        uname: global.uname,
        aname: global.aname,
        msize: global.msize,
        max_fids,
    })
}

fn serve(config: ServeConfig) -> CliResult<()> {
    match config.bind.clone() {
        BindTarget::Tcp(address) => serve_tcp(config, address),
        BindTarget::Unix(path) => serve_unix(config, path),
    }
}

fn serve_tcp(config: ServeConfig, address: SocketAddr) -> CliResult<()> {
    let listener = TcpListener::bind(address)
        .map_err(|error| cli_error(format!("bind {address}: {error}")))?;
    eprintln!(
        "r9p: serving {} on {}",
        config.root.display(),
        listener.local_addr()?
    );
    for stream in listener.incoming() {
        let stream =
            stream.map_err(|error| cli_error(format!("accept TCP connection: {error}")))?;
        let session = config.clone();
        thread::spawn(move || {
            if let Err(error) = serve_connection(stream, session) {
                eprintln!("r9p: serve connection: {error}");
            }
        });
    }
    Ok(())
}

fn serve_unix(config: ServeConfig, path: PathBuf) -> CliResult<()> {
    remove_stale_socket(&path)?;
    let listener = UnixListener::bind(&path)
        .map_err(|error| cli_error(format!("bind {}: {error}", path.display())))?;
    eprintln!(
        "r9p: serving {} on unix:{}",
        config.root.display(),
        path.display()
    );
    for stream in listener.incoming() {
        let stream =
            stream.map_err(|error| cli_error(format!("accept unix connection: {error}")))?;
        let session = config.clone();
        thread::spawn(move || {
            if let Err(error) = serve_connection(stream, session) {
                eprintln!("r9p: serve connection: {error}");
            }
        });
    }
    Ok(())
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
        .find(|address| address.ip().is_loopback())
        .ok_or_else(|| cli_error("r9p serve only admits loopback TCP binds in Plan 47"))?;
    Ok(BindTarget::Tcp(address))
}

fn default_unix_bind() -> BindTarget {
    BindTarget::Unix(std::env::temp_dir().join(format!("r9p-serve-{}.sock", std::process::id())))
}

fn serve_usage(code: i32) -> ! {
    eprintln!("usage: r9p serve [--bind address] [--max-fids count] root");
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::{parse_serve_config, BindTarget};
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
}
