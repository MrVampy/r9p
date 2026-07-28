use std::{path::PathBuf, time::Duration};

use crate::{
    commands::session_mount::{start_session_mount, take_session_mount_config},
    errors::{cli_error, CliResult},
    target::Config,
    usage,
};
use session::control::{
    request_control_socket, serve_control_socket_with_runtime, ControlConfig, ControlRuntime,
};

pub(crate) fn session_cmd(config: Config, mut args: Vec<String>) -> CliResult<()> {
    if args.is_empty() {
        usage();
    }
    let command = args.remove(0);
    match command.as_str() {
        "serve" => session_serve_cmd(config, args),
        "status" => session_status_cmd(config, args),
        "snapshot" => session_snapshot_cmd(config, args),
        "stat" => session_path_request_cmd(config, args, "stat", Some("/")),
        "list" => session_path_request_cmd(config, args, "list", Some("/")),
        "read" => session_path_request_cmd(config, args, "read", None),
        _ => Err(cli_error(format!("unknown session command {command}"))),
    }
}

fn session_serve_cmd(config: Config, mut args: Vec<String>) -> CliResult<()> {
    let socket = take_socket(&mut args)?;
    let change_feed_path = take_change_feed_path(&mut args)?;
    let change_feed_stream_path = take_change_feed_stream_path(&mut args)?;
    let change_feed_cursor_template = take_change_feed_cursor_template(&mut args)?;
    let change_feed_reconnect_delay = take_change_feed_reconnect_delay(&mut args)?;
    let change_feed_backpressure_limit = take_change_feed_backpressure(&mut args)?;
    let mount = take_session_mount_config(&mut args)?;
    let address = match (config.address.clone(), args.as_slice()) {
        (_, [endpoint]) => {
            let endpoint = endpoint.clone();
            args.clear();
            endpoint
        }
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
        auth_config: config.auth_config,
        authorities: config.authorities,
        connect_timeout: timeout_or_default(config.request_timeout),
        request_timeout: timeout_or_default(config.control_timeout.or(config.request_timeout)),
        change_feed_path,
        change_feed_stream_path,
        change_feed_cursor_template,
        change_feed_reconnect_delay,
        change_feed_backpressure_limit,
    };
    if !args.is_empty() {
        return Err(cli_error(format!(
            "unexpected session serve argument {}",
            args[0]
        )));
    }
    let runtime = ControlRuntime::start(&control)?;
    let _mount = start_session_mount(&control, &runtime, &mount)?;
    serve_control_socket_with_runtime(&socket, control, runtime)?;
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

fn session_path_request_cmd(
    config: Config,
    mut args: Vec<String>,
    request_name: &str,
    default_path: Option<&str>,
) -> CliResult<()> {
    let socket = take_socket(&mut args)?;
    let path = session_path_arg(request_name, args, default_path)?;
    let request = format!("{request_name}\t{path}");
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

fn session_path_arg(
    request_name: &str,
    args: Vec<String>,
    default_path: Option<&str>,
) -> CliResult<String> {
    match (args.as_slice(), default_path) {
        ([], Some(path)) => Ok(path.to_string()),
        ([], None) => Err(cli_error(format!(
            "session {request_name} requires one namespace path"
        ))),
        ([path], _) => Ok(path.clone()),
        _ => Err(cli_error(format!(
            "session {request_name} takes at most one namespace path"
        ))),
    }
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

fn take_change_feed_path(args: &mut Vec<String>) -> CliResult<Option<String>> {
    take_optional_value(args, "--change-feed")
}

fn take_change_feed_stream_path(args: &mut Vec<String>) -> CliResult<Option<String>> {
    take_optional_value(args, "--change-feed-stream")
}

fn take_change_feed_cursor_template(args: &mut Vec<String>) -> CliResult<Option<String>> {
    let value = take_optional_value(args, "--change-feed-cursor-template")?;
    if let Some(template) = &value {
        if !template.contains("{event_id}") {
            return Err(cli_error(
                "change feed cursor template must include {event_id}",
            ));
        }
    }
    Ok(value)
}

fn take_change_feed_reconnect_delay(args: &mut Vec<String>) -> CliResult<Duration> {
    take_optional_value(args, "--change-feed-reconnect-delay")?
        .map(|value| parse_duration_secs(&value, "change feed reconnect delay"))
        .transpose()
        .map(|value| value.unwrap_or_else(|| Duration::from_secs(1)))
}

fn take_change_feed_backpressure(args: &mut Vec<String>) -> CliResult<usize> {
    take_optional_value(args, "--change-feed-backpressure")?
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|_| cli_error(format!("invalid change feed backpressure {value}")))
        })
        .transpose()
        .map(|value| value.unwrap_or(4096))
}

fn take_optional_value(args: &mut Vec<String>, name: &str) -> CliResult<Option<String>> {
    let mut value = None;
    let mut rest = Vec::new();
    let mut index = 0;
    let equals_prefix = format!("{name}=");
    while index < args.len() {
        let arg = &args[index];
        if let Some(current) = arg.strip_prefix(&equals_prefix) {
            value = Some(current.to_string());
            index += 1;
        } else if arg == name {
            let current = args
                .get(index + 1)
                .ok_or_else(|| cli_error(format!("missing value for {name}")))?;
            value = Some(current.clone());
            index += 2;
        } else {
            rest.push(arg.clone());
            index += 1;
        }
    }
    *args = rest;
    Ok(value)
}

fn parse_duration_secs(value: &str, name: &str) -> CliResult<Duration> {
    let seconds = value
        .parse::<f64>()
        .map_err(|_| cli_error(format!("invalid {name} {value}")))?;
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(cli_error(format!("invalid {name} {value}")));
    }
    Ok(Duration::from_secs_f64(seconds))
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
