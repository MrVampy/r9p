use std::{
    collections::BTreeMap,
    net::{SocketAddr, ToSocketAddrs},
    path::PathBuf,
};

use crate::{
    errors::{cli_error, CliResult},
    target::Config,
};
use r9p::export_descriptor::{AuthBoundary, EXPORT_FORMAT_V1};

const DEFAULT_MAX_FIDS: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ServeConfig {
    pub(super) root: PathBuf,
    pub(super) bind: BindTarget,
    pub(super) uname: String,
    pub(super) aname: String,
    pub(super) msize: u32,
    pub(super) max_fids: usize,
    pub(super) writable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum BindTarget {
    Tcp(SocketAddr),
    Unix(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExportConfig {
    pub(super) serve: ServeConfig,
    pub(super) descriptor_file: Option<PathBuf>,
    pub(super) auth: AuthBoundary,
    pub(super) auth_config: Option<PathBuf>,
    pub(super) extra_fields: BTreeMap<String, String>,
}

pub(super) fn parse_serve_config(global: Config, args: Vec<String>) -> CliResult<ServeConfig> {
    if global.address.is_some() {
        return Err(cli_error(
            "r9p serve uses --bind for its listen address; do not use global -a",
        ));
    }
    if global.auth_config.is_some() {
        return Err(cli_error(
            "r9p serve is loopback/local only and does not accept --auth-config; use r9p export",
        ));
    }

    let mut bind = None;
    let mut max_fids = DEFAULT_MAX_FIDS;
    let mut writable = false;
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
            "--writable" => {
                writable = true;
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
        writable,
    })
}

pub(super) fn parse_export_config(global: Config, args: Vec<String>) -> CliResult<ExportConfig> {
    if global.address.is_some() {
        return Err(cli_error(
            "r9p export uses --bind for its listen address; do not use global -a",
        ));
    }

    let mut bind = None;
    let mut max_fids = DEFAULT_MAX_FIDS;
    let mut writable = false;
    let mut descriptor_file = None;
    let mut descriptor_format = "machine".to_string();
    let mut auth_config = global.auth_config;
    let mut extra_fields = BTreeMap::new();
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
            "--writable" => {
                writable = true;
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
            "--auth-config" => {
                index += 1;
                if auth_config.is_some() {
                    return Err(cli_error("auth config already specified"));
                }
                auth_config = Some(PathBuf::from(
                    args.get(index)
                        .ok_or_else(|| cli_error("missing auth config path"))?,
                ));
            }
            "--descriptor-field" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| cli_error("missing descriptor field"))?;
                let (field, field_value) = parse_descriptor_field(value)?;
                if extra_fields.insert(field.clone(), field_value).is_some() {
                    return Err(cli_error(format!("duplicate descriptor field {field}")));
                }
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
    validate_export_bind(&bind, auth_config.is_some())?;
    let auth = match &auth_config {
        Some(path) => {
            let config = r9p_auth::ServerConfig::read(path)?;
            AuthBoundary::p9any_noise_xx(config.domain())?
        }
        None => AuthBoundary::none(),
    };

    Ok(ExportConfig {
        serve: ServeConfig {
            root: PathBuf::from(&positional[0]),
            bind,
            uname: global.uname,
            aname: global.aname,
            msize: global.msize,
            max_fids,
            writable,
        },
        descriptor_file,
        auth,
        auth_config,
        extra_fields,
    })
}

fn parse_descriptor_field(value: &str) -> CliResult<(String, String)> {
    let (field, field_value) = value.split_once('=').ok_or_else(|| {
        cli_error(format!(
            "invalid descriptor field {value}: expected key=value"
        ))
    })?;
    if field.is_empty() {
        return Err(cli_error("descriptor field name is empty"));
    }
    Ok((field.to_string(), field_value.to_string()))
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
                "r9p serve only admits loopback TCP binds; use r9p export --auth-config for an authenticated network boundary",
            ));
        }
    }
    Ok(())
}

fn validate_export_bind(bind: &BindTarget, authenticated: bool) -> CliResult<()> {
    match bind {
        BindTarget::Tcp(address) if address.ip().is_loopback() => Ok(()),
        BindTarget::Tcp(_) if authenticated => Ok(()),
        BindTarget::Tcp(_) => Err(cli_error(
            "r9p export requires --auth-config for non-loopback TCP binds",
        )),
        BindTarget::Unix(_) if authenticated => Err(cli_error(
            "r9p export cannot use p9any session auth for a unix socket",
        )),
        BindTarget::Unix(_) => Ok(()),
    }
}

fn serve_usage(code: i32) -> ! {
    eprintln!("usage: r9p serve [--bind address] [--max-fids count] [--writable] root");
    std::process::exit(code);
}

fn export_usage(code: i32) -> ! {
    eprintln!(
        "usage: r9p export [--bind address] [--max-fids count] [--writable] [--descriptor machine] [--descriptor-file path] [--auth-config path] [--descriptor-field key=value] root"
    );
    std::process::exit(code);
}
