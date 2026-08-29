use std::path::PathBuf;
use std::time::Duration;

use fuse::Config as MountConfig;

use crate::{
    errors::{cli_error, CliResult},
    target::Config,
};

const MAX_CONFIGURED_WORKERS: usize = 1024;
const MAX_CONFIGURED_BACKGROUND: u16 = 1024;

pub(super) fn parse_mount_config(global: Config, args: Vec<String>) -> CliResult<MountConfig> {
    if global.address.is_some() {
        return Err(cli_error(
            "r9p mount takes the endpoint as a positional argument; do not use global -a",
        ));
    }

    let authentication = crate::target::client_authentication(&global)?;
    let mut config = MountConfig {
        address: String::new(),
        fallback_addresses: Vec::new(),
        authentication,
        source_path: "/".to_string(),
        mountpoint: String::new(),
        uname: global.uname,
        aname: global.aname,
        msize: if global.msize_set {
            global.msize
        } else {
            r9p::codec::MAX_MSIZE
        },
        connect_timeout: Duration::from_secs(30),
        attr_timeout: fuse::DEFAULT_ATTR_TIMEOUT,
        entry_timeout: fuse::DEFAULT_ENTRY_TIMEOUT,
        negative_timeout: fuse::DEFAULT_NEGATIVE_TIMEOUT,
        request_timeout: Duration::from_secs(5),
        lookup_timeout: Duration::ZERO,
        read_timeout: Duration::ZERO,
        change_feed_read_timeout: Duration::ZERO,
        write_timeout: Duration::ZERO,
        mutation_timeout: Duration::ZERO,
        control_timeout: Duration::ZERO,
        interrupt_timeout: Duration::ZERO,
        max_workers: fuse::DEFAULT_MAX_WORKERS,
        max_background: fuse::DEFAULT_MAX_BACKGROUND,
        congestion_threshold: fuse::default_congestion_threshold(fuse::DEFAULT_MAX_BACKGROUND),
        diagnostics_path: None,
        diagnostics_capacity: 0,
        status_path: None,
        change_feed_path: None,
        change_feed_stream_path: None,
        change_feed_cursor_template: None,
        change_feed_scope: None,
        change_feed_reconnect_delay: Duration::ZERO,
        change_feed_backpressure_limit: 0,
        coherent_read_cache: false,
        allow_other: false,
        debug: false,
    };

    let mut congestion_threshold_set = false;
    let mut positional = Vec::new();
    let mut index = 0_usize;
    while index < args.len() {
        match args[index].as_str() {
            "-D" | "--debug" => config.debug = true,
            "--allow-other" => config.allow_other = true,
            "--coherent-read-cache" => config.coherent_read_cache = true,
            "--source" => {
                index += 1;
                config.source_path = args
                    .get(index)
                    .ok_or_else(|| cli_error("missing mount source path"))?
                    .clone();
            }
            "--fallback-endpoint" => {
                index += 1;
                config.fallback_addresses.push(
                    args.get(index)
                        .ok_or_else(|| cli_error("missing fallback endpoint"))?
                        .clone(),
                );
            }
            "--attr-timeout" => {
                index += 1;
                config.attr_timeout = parse_duration(args.get(index), "missing attr timeout")?;
            }
            "--entry-timeout" => {
                index += 1;
                config.entry_timeout = parse_duration(args.get(index), "missing entry timeout")?;
            }
            "--negative-timeout" => {
                index += 1;
                config.negative_timeout =
                    parse_duration(args.get(index), "missing negative timeout")?;
            }
            "--request-timeout" => {
                index += 1;
                config.request_timeout =
                    parse_duration(args.get(index), "missing request timeout")?;
            }
            "--connect-timeout" => {
                index += 1;
                config.connect_timeout =
                    parse_duration(args.get(index), "missing connect timeout")?;
            }
            "--lookup-timeout" => {
                index += 1;
                config.lookup_timeout = parse_duration(args.get(index), "missing lookup timeout")?;
            }
            "--read-timeout" => {
                index += 1;
                config.read_timeout = parse_duration(args.get(index), "missing read timeout")?;
            }
            "--change-feed-read-timeout" => {
                index += 1;
                config.change_feed_read_timeout =
                    parse_duration(args.get(index), "missing change feed read timeout")?;
            }
            "--write-timeout" => {
                index += 1;
                config.write_timeout = parse_duration(args.get(index), "missing write timeout")?;
            }
            "--mutation-timeout" => {
                index += 1;
                config.mutation_timeout =
                    parse_duration(args.get(index), "missing mutation timeout")?;
            }
            "--control-timeout" => {
                index += 1;
                config.control_timeout =
                    parse_duration(args.get(index), "missing control timeout")?;
            }
            "--interrupt-timeout" => {
                index += 1;
                config.interrupt_timeout =
                    parse_duration(args.get(index), "missing interrupt timeout")?;
            }
            "--diagnostics-file" => {
                index += 1;
                config.diagnostics_path = Some(PathBuf::from(
                    args.get(index)
                        .ok_or_else(|| cli_error("missing diagnostics file"))?,
                ));
            }
            "--diagnostics-capacity" => {
                index += 1;
                config.diagnostics_capacity = parse_usize_limit(
                    args.get(index),
                    "missing diagnostics capacity",
                    "diagnostics capacity",
                    65_536,
                )?;
            }
            "--status-file" => {
                index += 1;
                config.status_path = Some(PathBuf::from(
                    args.get(index)
                        .ok_or_else(|| cli_error("missing status file"))?,
                ));
            }
            "--change-feed" => {
                index += 1;
                config.change_feed_path = Some(
                    args.get(index)
                        .ok_or_else(|| cli_error("missing change feed path"))?
                        .clone(),
                );
            }
            "--change-feed-stream" => {
                index += 1;
                config.change_feed_stream_path = Some(
                    args.get(index)
                        .ok_or_else(|| cli_error("missing change feed stream path"))?
                        .clone(),
                );
            }
            "--change-feed-cursor-template" => {
                index += 1;
                let template = args
                    .get(index)
                    .ok_or_else(|| cli_error("missing change feed cursor template"))?;
                if !template.contains("{event_id}") {
                    return Err(cli_error(
                        "change feed cursor template must include {event_id}",
                    ));
                }
                config.change_feed_cursor_template = Some(template.clone());
            }
            "--change-feed-scope" => {
                index += 1;
                config.change_feed_scope = Some(
                    args.get(index)
                        .ok_or_else(|| cli_error("missing change feed scope"))?
                        .clone(),
                );
            }
            "--change-feed-reconnect-delay" => {
                index += 1;
                config.change_feed_reconnect_delay =
                    parse_duration(args.get(index), "missing change feed reconnect delay")?;
            }
            "--change-feed-backpressure" => {
                index += 1;
                config.change_feed_backpressure_limit = parse_usize_limit(
                    args.get(index),
                    "missing change feed backpressure limit",
                    "change feed backpressure limit",
                    1_000_000,
                )?;
            }
            "--max-workers" => {
                index += 1;
                config.max_workers = parse_usize_limit(
                    args.get(index),
                    "missing max workers",
                    "max workers",
                    MAX_CONFIGURED_WORKERS,
                )?;
            }
            "--max-background" => {
                index += 1;
                config.max_background = parse_u16_limit(
                    args.get(index),
                    "missing max background",
                    "max background",
                    MAX_CONFIGURED_BACKGROUND,
                )?;
                if !congestion_threshold_set {
                    config.congestion_threshold =
                        fuse::default_congestion_threshold(config.max_background);
                }
            }
            "--congestion-threshold" => {
                index += 1;
                config.congestion_threshold = parse_u16_limit(
                    args.get(index),
                    "missing congestion threshold",
                    "congestion threshold",
                    MAX_CONFIGURED_BACKGROUND,
                )?;
                congestion_threshold_set = true;
            }
            "-A" | "--aname" => {
                index += 1;
                config.aname = args
                    .get(index)
                    .ok_or_else(|| cli_error("missing aname"))?
                    .clone();
            }
            "-u" | "--uname" => {
                index += 1;
                config.uname = args
                    .get(index)
                    .ok_or_else(|| cli_error("missing uname"))?
                    .clone();
            }
            "-m" | "--msize" => {
                index += 1;
                let value = args.get(index).ok_or_else(|| cli_error("missing msize"))?;
                config.msize = value
                    .parse::<u32>()
                    .map_err(|_| cli_error(format!("invalid msize {value}")))?;
            }
            "-a" => {
                return Err(cli_error(
                    "r9p mount uses --aname or -A for aname; -a is not accepted here",
                ));
            }
            "-h" | "--help" => mount_usage(0),
            arg if arg.starts_with('-') => {
                return Err(cli_error(format!("unknown mount option {arg}")));
            }
            arg => positional.push(arg.to_string()),
        }
        index += 1;
    }

    if positional.len() != 2 {
        return Err(cli_error("expected endpoint and mountpoint"));
    }
    if config.congestion_threshold > config.max_background {
        return Err(cli_error(
            "congestion threshold must be less than or equal to max background",
        ));
    }
    config.address = positional[0].clone();
    config.mountpoint = positional[1].clone();
    Ok(config)
}

fn parse_duration(value: Option<&String>, missing: &'static str) -> CliResult<Duration> {
    let value = value.ok_or_else(|| cli_error(missing))?;
    let seconds = value
        .parse::<f64>()
        .map_err(|_| cli_error(format!("invalid duration {value}")))?;
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(cli_error(format!("invalid duration {value}")));
    }
    Ok(Duration::from_secs_f64(seconds))
}

fn parse_usize_limit(
    value: Option<&String>,
    missing: &'static str,
    label: &'static str,
    limit: usize,
) -> CliResult<usize> {
    let value = value.ok_or_else(|| cli_error(missing))?;
    let parsed = value
        .parse::<usize>()
        .map_err(|_| cli_error(format!("invalid {label} {value}")))?;
    if parsed == 0 || parsed > limit {
        return Err(cli_error(format!(
            "{label} must be between 1 and {limit}: {value}"
        )));
    }
    Ok(parsed)
}

fn parse_u16_limit(
    value: Option<&String>,
    missing: &'static str,
    label: &'static str,
    limit: u16,
) -> CliResult<u16> {
    let value = value.ok_or_else(|| cli_error(missing))?;
    let parsed = value
        .parse::<u16>()
        .map_err(|_| cli_error(format!("invalid {label} {value}")))?;
    if parsed == 0 || parsed > limit {
        return Err(cli_error(format!(
            "{label} must be between 1 and {limit}: {value}"
        )));
    }
    Ok(parsed)
}

fn mount_usage(code: i32) -> ! {
    eprintln!(
        "usage: r9p mount [--fallback-endpoint endpoint ...] [--source namespace-path] [--aname aname] [--uname uname] [--msize msize] [--allow-other] [--coherent-read-cache] [--attr-timeout seconds] [--entry-timeout seconds] [--negative-timeout seconds] [--request-timeout seconds] [--connect-timeout seconds] [--lookup-timeout seconds] [--read-timeout seconds] [--change-feed-read-timeout seconds] [--write-timeout seconds] [--mutation-timeout seconds] [--control-timeout seconds] [--interrupt-timeout seconds] [--max-workers count] [--max-background count] [--congestion-threshold count] [--diagnostics-file path] [--diagnostics-capacity count] [--status-file path] [--change-feed namespace-path] [--change-feed-stream namespace-path] [--change-feed-cursor-template path-with-{{event_id}}] [--change-feed-scope scope] [--change-feed-reconnect-delay seconds] [--change-feed-backpressure count] endpoint mountpoint\nusage: r9p mount ensure|status|stop --mountpoint path [--unit name --unit-scope user|system] [--status-file path] [--expect-endpoint endpoint] [--expect-change-feed path] [--expect-status-file path] [--attempts count] [-- mount args...]\nusage: r9p mount read-ahead --mountpoint path --kilobytes count [--attempts count]"
    );
    std::process::exit(code);
}
