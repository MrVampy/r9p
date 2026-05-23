use std::path::PathBuf;
use std::time::Duration;

use fuse::Config as MountConfig;

use crate::{
    errors::{cli_error, CliResult},
    target::Config,
};

const MAX_CONFIGURED_WORKERS: usize = 1024;
const MAX_CONFIGURED_BACKGROUND: u16 = 1024;

pub(crate) fn mount_cmd(global: Config, args: Vec<String>) -> CliResult<()> {
    let config = parse_mount_config(global, args)?;
    fuse::mount(config).map_err(|error| cli_error(format!("mount: {}", error.message())))
}

pub(crate) fn parse_mount_config(global: Config, args: Vec<String>) -> CliResult<MountConfig> {
    if global.address.is_some() {
        return Err(cli_error(
            "r9p mount takes the endpoint as a positional argument; do not use global -a",
        ));
    }

    let mut config = MountConfig {
        address: String::new(),
        mountpoint: String::new(),
        uname: global.uname,
        aname: global.aname,
        msize: if global.msize_set {
            global.msize
        } else {
            r9p::codec::MAX_MSIZE
        },
        attr_timeout: fuse::DEFAULT_ATTR_TIMEOUT,
        entry_timeout: fuse::DEFAULT_ENTRY_TIMEOUT,
        request_timeout: Duration::from_secs(5),
        lookup_timeout: Duration::ZERO,
        read_timeout: Duration::ZERO,
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
        change_feed_scope: None,
        change_feed_poll_interval: Duration::ZERO,
        change_feed_backpressure_limit: 0,
        debug: false,
    };

    let mut congestion_threshold_set = false;
    let mut positional = Vec::new();
    let mut index = 0_usize;
    while index < args.len() {
        match args[index].as_str() {
            "-D" | "--debug" => config.debug = true,
            "--attr-timeout" => {
                index += 1;
                config.attr_timeout = parse_duration(args.get(index), "missing attr timeout")?;
            }
            "--entry-timeout" => {
                index += 1;
                config.entry_timeout = parse_duration(args.get(index), "missing entry timeout")?;
            }
            "--request-timeout" => {
                index += 1;
                config.request_timeout =
                    parse_duration(args.get(index), "missing request timeout")?;
            }
            "--lookup-timeout" => {
                index += 1;
                config.lookup_timeout = parse_duration(args.get(index), "missing lookup timeout")?;
            }
            "--read-timeout" => {
                index += 1;
                config.read_timeout = parse_duration(args.get(index), "missing read timeout")?;
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
            "--change-feed-scope" => {
                index += 1;
                config.change_feed_scope = Some(
                    args.get(index)
                        .ok_or_else(|| cli_error("missing change feed scope"))?
                        .clone(),
                );
            }
            "--change-feed-poll-interval" => {
                index += 1;
                config.change_feed_poll_interval =
                    parse_duration(args.get(index), "missing change feed poll interval")?;
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
        "usage: r9p mount [--aname aname] [--uname uname] [--msize msize] [--attr-timeout seconds] [--entry-timeout seconds] [--request-timeout seconds] [--lookup-timeout seconds] [--read-timeout seconds] [--write-timeout seconds] [--mutation-timeout seconds] [--control-timeout seconds] [--interrupt-timeout seconds] [--max-workers count] [--max-background count] [--congestion-threshold count] [--diagnostics-file path] [--diagnostics-capacity count] [--status-file path] [--change-feed namespace-path] [--change-feed-scope scope] [--change-feed-poll-interval seconds] [--change-feed-backpressure count] endpoint mountpoint"
    );
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::parse_mount_config;
    use crate::{target::Config, DEFAULT_MSIZE};

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
    fn parses_final_mount_options() {
        let config = parse_mount_config(
            global(),
            vec![
                "--uname".to_string(),
                "glenda".to_string(),
                "--aname".to_string(),
                "/".to_string(),
                "--request-timeout".to_string(),
                "0.25".to_string(),
                "--lookup-timeout".to_string(),
                "0.5".to_string(),
                "--read-timeout".to_string(),
                "1".to_string(),
                "--write-timeout".to_string(),
                "2".to_string(),
                "--mutation-timeout".to_string(),
                "3".to_string(),
                "--control-timeout".to_string(),
                "4".to_string(),
                "--interrupt-timeout".to_string(),
                "0.125".to_string(),
                "--diagnostics-file".to_string(),
                "/tmp/r9p-mount-diagnostics.jsonl".to_string(),
                "--diagnostics-capacity".to_string(),
                "64".to_string(),
                "--status-file".to_string(),
                "/tmp/r9p-mount-status.json".to_string(),
                "--change-feed".to_string(),
                "/runtime/events/namespace/stream".to_string(),
                "--change-feed-scope".to_string(),
                "session:mount-a".to_string(),
                "--change-feed-poll-interval".to_string(),
                "0.75".to_string(),
                "--change-feed-backpressure".to_string(),
                "128".to_string(),
                "--max-workers".to_string(),
                "8".to_string(),
                "--max-background".to_string(),
                "24".to_string(),
                "--congestion-threshold".to_string(),
                "18".to_string(),
                "--attr-timeout".to_string(),
                "1.5".to_string(),
                "--entry-timeout".to_string(),
                "2".to_string(),
                "--msize".to_string(),
                "8192".to_string(),
                "127.0.0.1:564".to_string(),
                "/tmp/r9p-mount".to_string(),
            ],
        )
        .expect("mount options should parse");

        assert_eq!(config.uname, "glenda");
        assert_eq!(config.aname, "/");
        assert_eq!(config.address, "127.0.0.1:564");
        assert_eq!(config.mountpoint, "/tmp/r9p-mount");
        assert_eq!(config.request_timeout, Duration::from_millis(250));
        assert_eq!(config.lookup_timeout, Duration::from_millis(500));
        assert_eq!(config.read_timeout, Duration::from_secs(1));
        assert_eq!(config.write_timeout, Duration::from_secs(2));
        assert_eq!(config.mutation_timeout, Duration::from_secs(3));
        assert_eq!(config.control_timeout, Duration::from_secs(4));
        assert_eq!(config.interrupt_timeout, Duration::from_millis(125));
        assert_eq!(
            config.diagnostics_path.as_deref(),
            Some(std::path::Path::new("/tmp/r9p-mount-diagnostics.jsonl"))
        );
        assert_eq!(config.diagnostics_capacity, 64);
        assert_eq!(
            config.status_path.as_deref(),
            Some(std::path::Path::new("/tmp/r9p-mount-status.json"))
        );
        assert_eq!(
            config.change_feed_path.as_deref(),
            Some("/runtime/events/namespace/stream")
        );
        assert_eq!(config.change_feed_scope.as_deref(), Some("session:mount-a"));
        assert_eq!(config.change_feed_poll_interval, Duration::from_millis(750));
        assert_eq!(config.change_feed_backpressure_limit, 128);
        assert_eq!(config.attr_timeout, Duration::from_millis(1500));
        assert_eq!(config.entry_timeout, Duration::from_secs(2));
        assert_eq!(config.max_workers, 8);
        assert_eq!(config.max_background, 24);
        assert_eq!(config.congestion_threshold, 18);
        assert_eq!(config.msize, 8192);
    }

    #[test]
    fn mount_defaults_use_short_positive_kernel_cache() {
        let config = parse_mount_config(
            global(),
            vec!["127.0.0.1:564".to_string(), "/tmp/r9p-mount".to_string()],
        )
        .expect("mount options should parse");

        assert_eq!(config.attr_timeout, fuse::DEFAULT_ATTR_TIMEOUT);
        assert_eq!(config.entry_timeout, fuse::DEFAULT_ENTRY_TIMEOUT);
    }

    #[test]
    fn mount_allows_explicit_zero_kernel_cache() {
        let config = parse_mount_config(
            global(),
            vec![
                "--attr-timeout".to_string(),
                "0".to_string(),
                "--entry-timeout".to_string(),
                "0".to_string(),
                "127.0.0.1:564".to_string(),
                "/tmp/r9p-mount".to_string(),
            ],
        )
        .expect("mount options should parse");

        assert_eq!(config.attr_timeout, Duration::ZERO);
        assert_eq!(config.entry_timeout, Duration::ZERO);
    }

    #[test]
    fn derives_congestion_threshold_from_max_background() {
        let config = parse_mount_config(
            global(),
            vec![
                "--max-background".to_string(),
                "16".to_string(),
                "127.0.0.1:564".to_string(),
                "/tmp/r9p-mount".to_string(),
            ],
        )
        .expect("mount options should parse");

        assert_eq!(config.max_background, 16);
        assert_eq!(config.congestion_threshold, 12);
    }

    #[test]
    fn rejects_unbounded_worker_and_queue_knobs() {
        for args in [
            vec![
                "--max-workers".to_string(),
                "0".to_string(),
                "127.0.0.1:564".to_string(),
                "/tmp/r9p-mount".to_string(),
            ],
            vec![
                "--max-background".to_string(),
                "2048".to_string(),
                "127.0.0.1:564".to_string(),
                "/tmp/r9p-mount".to_string(),
            ],
            vec![
                "--max-background".to_string(),
                "4".to_string(),
                "--congestion-threshold".to_string(),
                "8".to_string(),
                "127.0.0.1:564".to_string(),
                "/tmp/r9p-mount".to_string(),
            ],
            vec![
                "--change-feed-backpressure".to_string(),
                "0".to_string(),
                "127.0.0.1:564".to_string(),
                "/tmp/r9p-mount".to_string(),
            ],
        ] {
            assert!(parse_mount_config(global(), args).is_err());
        }
    }

    #[test]
    fn rejects_old_mount_short_options() {
        for option in ["-a", "-E", "-T"] {
            let result = parse_mount_config(
                global(),
                vec![
                    option.to_string(),
                    "1".to_string(),
                    "127.0.0.1:564".to_string(),
                    "/tmp/r9p-mount".to_string(),
                ],
            );
            assert!(result.is_err(), "{option} should not parse");
        }
    }

    #[test]
    fn dash_upper_a_is_aname_not_attr_timeout() {
        let config = parse_mount_config(
            global(),
            vec![
                "-A".to_string(),
                "/".to_string(),
                "127.0.0.1:564".to_string(),
                "/tmp/r9p-mount".to_string(),
            ],
        )
        .expect("mount options should parse");

        assert_eq!(config.aname, "/");
        assert_eq!(config.attr_timeout, fuse::DEFAULT_ATTR_TIMEOUT);
    }

    #[test]
    fn mount_rejects_global_address_option() {
        let mut global = global();
        global.address = Some("127.0.0.1:564".to_string());
        let result = parse_mount_config(global, vec!["/tmp/r9p-mount".to_string()]);

        assert!(result.is_err());
    }
}
