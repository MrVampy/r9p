use std::{collections::BTreeSet, net::SocketAddr, path::PathBuf};

use crate::{
    commands::listener,
    errors::{cli_error, CliResult},
    target::Config,
};

const DEFAULT_MAX_SESSIONS: usize = 4;
const DEFAULT_MAX_BUFFER_BYTES: usize = 1024 * 1024;
const MAX_SESSIONS: usize = 64;
const MIN_BUFFER_BYTES: usize = 4096;
const MAX_BUFFER_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ProcessCommand {
    pub(super) program: PathBuf,
    pub(super) arguments: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct StreamExportConfig {
    pub(super) bind: SocketAddr,
    pub(super) auth_config: PathBuf,
    pub(super) allowed_principals: BTreeSet<String>,
    pub(super) max_sessions: usize,
    pub(super) max_buffer_bytes: usize,
    pub(super) status_file: Option<PathBuf>,
    pub(super) command: ProcessCommand,
    pub(super) uname: String,
    pub(super) aname: String,
    pub(super) msize: u32,
}

pub(super) fn parse(global: Config, args: Vec<String>) -> CliResult<StreamExportConfig> {
    if global.address.is_some() {
        return Err(cli_error(
            "r9p stream-export uses --bind for its listen address; do not use global -a",
        ));
    }
    if !matches!(global.aname.as_str(), "" | "/") {
        return Err(cli_error("r9p stream-export serves only attach name /"));
    }

    let mut bind = None;
    let mut auth_config = global.auth_config;
    let mut allowed_principals = BTreeSet::new();
    let mut max_sessions = DEFAULT_MAX_SESSIONS;
    let mut max_buffer_bytes = DEFAULT_MAX_BUFFER_BYTES;
    let mut status_file = None;
    let mut command = Vec::new();
    let mut index = 0_usize;
    while index < args.len() {
        match args[index].as_str() {
            "--" => {
                command.extend(args[index + 1..].iter().cloned());
                break;
            }
            "--bind" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| cli_error("missing bind address"))?;
                if bind.replace(listener::parse_tcp_bind(value)?).is_some() {
                    return Err(cli_error("bind address already specified"));
                }
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
            "--allow-principal" => {
                index += 1;
                let principal = args
                    .get(index)
                    .ok_or_else(|| cli_error("missing allowed principal"))?;
                validate_principal(principal)?;
                if !allowed_principals.insert(principal.clone()) {
                    return Err(cli_error(format!(
                        "duplicate allowed principal {principal}"
                    )));
                }
            }
            "--max-sessions" => {
                index += 1;
                max_sessions = parse_usize(
                    args.get(index)
                        .ok_or_else(|| cli_error("missing max session count"))?,
                    "max session count",
                )?;
            }
            "--max-buffer-bytes" => {
                index += 1;
                max_buffer_bytes = parse_usize(
                    args.get(index)
                        .ok_or_else(|| cli_error("missing max buffer byte count"))?,
                    "max buffer byte count",
                )?;
            }
            "--status-file" => {
                index += 1;
                if status_file
                    .replace(PathBuf::from(
                        args.get(index)
                            .ok_or_else(|| cli_error("missing status file"))?,
                    ))
                    .is_some()
                {
                    return Err(cli_error("status file already specified"));
                }
            }
            option if option.starts_with('-') => {
                return Err(cli_error(format!("unknown stream-export option {option}")));
            }
            _ => {
                return Err(cli_error("stream-export process command must follow --"));
            }
        }
        index += 1;
    }

    let bind = bind.ok_or_else(|| cli_error("missing --bind"))?;
    let auth_config = auth_config.ok_or_else(|| cli_error("missing --auth-config"))?;
    if allowed_principals.is_empty() {
        return Err(cli_error("missing --allow-principal"));
    }
    if !(1..=MAX_SESSIONS).contains(&max_sessions) {
        return Err(cli_error(format!(
            "max session count must be between 1 and {MAX_SESSIONS}"
        )));
    }
    if !(MIN_BUFFER_BYTES..=MAX_BUFFER_BYTES).contains(&max_buffer_bytes) {
        return Err(cli_error(format!(
            "max buffer byte count must be between {MIN_BUFFER_BYTES} and {MAX_BUFFER_BYTES}"
        )));
    }
    let (program, arguments) = command
        .split_first()
        .ok_or_else(|| cli_error("missing stream-export process command after --"))?;
    let program = PathBuf::from(program);
    if !program.is_absolute() {
        return Err(cli_error(
            "stream-export process command must use an absolute executable path",
        ));
    }

    Ok(StreamExportConfig {
        bind,
        auth_config,
        allowed_principals,
        max_sessions,
        max_buffer_bytes,
        status_file,
        command: ProcessCommand {
            program,
            arguments: arguments.to_vec(),
        },
        uname: global.uname,
        aname: "/".to_string(),
        msize: global.msize,
    })
}

fn validate_principal(principal: &str) -> CliResult<()> {
    if principal.is_empty()
        || principal.len() > 255
        || principal
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
    {
        return Err(cli_error(format!("invalid allowed principal {principal}")));
    }
    Ok(())
}

fn parse_usize(value: &str, label: &str) -> CliResult<usize> {
    value
        .parse::<usize>()
        .map_err(|_| cli_error(format!("invalid {label} {value}")))
}
