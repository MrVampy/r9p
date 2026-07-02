use std::{path::PathBuf, time::Duration};

use crate::{
    errors::{cli_error, CliResult},
    target::Config,
    usage,
};
use session::control::{request_control_socket, serve_control_socket, ControlConfig};

pub(crate) fn session_cmd(config: Config, mut args: Vec<String>) -> CliResult<()> {
    if args.is_empty() {
        usage();
    }
    let command = args.remove(0);
    match command.as_str() {
        "serve" => session_serve_cmd(config, args),
        "status" => session_status_cmd(config, args),
        "snapshot" => session_snapshot_cmd(config, args),
        _ => Err(cli_error(format!("unknown session command {command}"))),
    }
}

fn session_serve_cmd(config: Config, mut args: Vec<String>) -> CliResult<()> {
    let socket = take_socket(&mut args)?;
    let address = match (config.address.clone(), args.as_slice()) {
        (_, [endpoint]) => endpoint.clone(),
        (Some(address), []) => address,
        _ => {
            return Err(cli_error(
                "session serve requires --socket PATH and endpoint or -a",
            ))
        }
    };
    let control = ControlConfig {
        address,
        uname: config.uname,
        aname: config.aname,
        msize: config.msize,
        connect_timeout: timeout_or_default(config.request_timeout),
        request_timeout: timeout_or_default(config.control_timeout.or(config.request_timeout)),
    };
    serve_control_socket(&socket, control)?;
    Ok(())
}

fn session_status_cmd(config: Config, mut args: Vec<String>) -> CliResult<()> {
    let socket = take_socket(&mut args)?;
    if !args.is_empty() {
        return Err(cli_error("session status takes no path arguments"));
    }
    let response = request_control_socket(
        &socket,
        "status",
        timeout_or_default(config.request_timeout),
    )?;
    print_response(&response);
    Ok(())
}

fn session_snapshot_cmd(config: Config, mut args: Vec<String>) -> CliResult<()> {
    let socket = take_socket(&mut args)?;
    let depth = take_depth(&mut args)?;
    let path = match args.as_slice() {
        [] => "/".to_string(),
        [path] => path.clone(),
        _ => return Err(cli_error("session snapshot takes at most one path")),
    };
    let request = format!("snapshot\t{path}\t{depth}");
    let response = request_control_socket(
        &socket,
        &request,
        timeout_or_default(config.request_timeout),
    )?;
    print_response(&response);
    Ok(())
}

fn take_socket(args: &mut Vec<String>) -> CliResult<PathBuf> {
    let mut socket = None;
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if let Some(value) = arg.strip_prefix("--socket=") {
            socket = Some(PathBuf::from(value));
            i += 1;
        } else if arg == "--socket" {
            let value = args
                .get(i + 1)
                .ok_or_else(|| cli_error("missing value for --socket"))?;
            socket = Some(PathBuf::from(value));
            i += 2;
        } else {
            rest.push(arg.clone());
            i += 1;
        }
    }
    *args = rest;
    socket.ok_or_else(|| cli_error("missing --socket PATH"))
}

fn take_depth(args: &mut Vec<String>) -> CliResult<usize> {
    let mut depth = 1;
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if let Some(value) = arg.strip_prefix("--depth=") {
            depth = parse_depth(value)?;
            i += 1;
        } else if arg == "--depth" {
            let value = args
                .get(i + 1)
                .ok_or_else(|| cli_error("missing value for --depth"))?;
            depth = parse_depth(value)?;
            i += 2;
        } else {
            rest.push(arg.clone());
            i += 1;
        }
    }
    *args = rest;
    Ok(depth)
}

fn parse_depth(value: &str) -> CliResult<usize> {
    value
        .parse()
        .map_err(|_| cli_error(format!("invalid session snapshot depth {value}")))
}

fn timeout_or_default(timeout: Option<Duration>) -> Duration {
    timeout.unwrap_or_else(|| Duration::from_secs(30))
}

fn print_response(response: &str) {
    print!("{response}");
    if !response.ends_with('\n') {
        println!();
    }
}
