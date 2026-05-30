use std::env;
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use fuse::Config as MountConfig;

use crate::{
    errors::{cli_error, CliResult},
    target::Config,
};

const MAX_CONFIGURED_WORKERS: usize = 1024;
const MAX_CONFIGURED_BACKGROUND: u16 = 1024;

pub(crate) fn mount_cmd(global: Config, args: Vec<String>) -> CliResult<()> {
    if let Some(action) = args.first().map(String::as_str) {
        match action {
            "ensure" => return mount_ensure_cmd(args[1..].to_vec()),
            "status" => return mount_status_cmd(args[1..].to_vec()),
            "stop" => return mount_stop_cmd(args[1..].to_vec()),
            _ => {}
        }
    }
    let config = parse_mount_config(global, args)?;
    fuse::mount(config).map_err(|error| cli_error(format!("mount: {}", error.message())))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MountSupervisorConfig {
    mountpoint: PathBuf,
    unit: Option<String>,
    expected_endpoint: Option<String>,
    expected_status_file: Option<String>,
    expected_change_feed: Option<String>,
    status_file: Option<PathBuf>,
    attempts: usize,
}

fn mount_status_cmd(args: Vec<String>) -> CliResult<()> {
    let config = parse_mount_supervisor_config(args)?;
    check_mount_status(&config)?;
    if let Some(unit) = &config.unit {
        println!("unit {unit} ready");
    }
    println!("mountpoint {} ready", config.mountpoint.display());
    if let Some(status_file) = &config.status_file {
        match std::fs::read_to_string(status_file) {
            Ok(content) => println!("status {}", content.trim()),
            Err(error) => println!("status unavailable {}: {error}", status_file.display()),
        }
    }
    Ok(())
}

fn mount_ensure_cmd(args: Vec<String>) -> CliResult<()> {
    let (config, mount_args) = parse_mount_ensure_config(args)?;
    if check_mount_status(&config).is_ok() {
        println!("mountpoint {} ready", config.mountpoint.display());
        return Ok(());
    }
    stop_mount(&config)?;
    std::fs::create_dir_all(&config.mountpoint).map_err(|error| {
        cli_error(format!(
            "r9p_mount_mkdir_failed:{}:{error}",
            config.mountpoint.display()
        ))
    })?;
    start_systemd_mount(&config, &mount_args)?;
    wait_for_mount_status(&config)?;
    println!("mountpoint {} mounted", config.mountpoint.display());
    Ok(())
}

fn mount_stop_cmd(args: Vec<String>) -> CliResult<()> {
    let config = parse_mount_supervisor_config(args)?;
    stop_mount(&config)?;
    println!("mountpoint {} stopped", config.mountpoint.display());
    Ok(())
}

fn check_mount_status(config: &MountSupervisorConfig) -> CliResult<()> {
    assert_single_mount_layer(&config.mountpoint)?;
    if let Some(unit) = &config.unit {
        let unit_command = systemd_unit_command(unit)?;
        assert_unit_command_contains(
            &unit_command,
            config.expected_endpoint.as_deref(),
            "endpoint",
        )?;
        assert_unit_command_contains(
            &unit_command,
            config.expected_change_feed.as_deref(),
            "change_feed",
        )?;
        assert_unit_command_contains(
            &unit_command,
            config.expected_status_file.as_deref(),
            "status_file",
        )?;
    }
    Ok(())
}

fn stop_mount(config: &MountSupervisorConfig) -> CliResult<()> {
    if let Some(unit) = &config.unit {
        let _ = Command::new("systemctl")
            .args(["--user", "stop", unit])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    unmount_mountpoint_layers(&config.mountpoint, config.attempts)?;
    if let Some(unit) = &config.unit {
        let _ = Command::new("systemctl")
            .args(["--user", "reset-failed", unit])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    Ok(())
}

fn start_systemd_mount(config: &MountSupervisorConfig, mount_args: &[String]) -> CliResult<()> {
    let unit = config
        .unit
        .as_deref()
        .ok_or_else(|| cli_error("r9p mount ensure requires --unit"))?;
    let executable =
        env::current_exe().map_err(|error| cli_error(format!("resolve current r9p: {error}")))?;
    let mut command = Command::new("systemd-run");
    command.args(["--user", "--unit", unit, "--collect", "--same-dir"]);
    if let Ok(path) = env::var("PATH") {
        command.arg(format!("--setenv=PATH={path}"));
    }
    command.arg(executable).arg("mount").args(mount_args);
    let output = command
        .output()
        .map_err(|error| cli_error(format!("systemd-run {unit}: {error}")))?;
    if !output.status.success() {
        return Err(cli_error(format!(
            "systemd_run_failed:{unit}:stdout={:?}:stderr={:?}",
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

fn wait_for_mount_status(config: &MountSupervisorConfig) -> CliResult<()> {
    for attempt in 0..=config.attempts {
        match check_mount_status(config) {
            Ok(()) => return Ok(()),
            Err(error) if attempt >= config.attempts => return Err(error),
            Err(_) => thread::sleep(Duration::from_millis(100)),
        }
    }
    Ok(())
}

fn parse_mount_ensure_config(args: Vec<String>) -> CliResult<(MountSupervisorConfig, Vec<String>)> {
    let separator = args
        .iter()
        .position(|arg| arg == "--")
        .ok_or_else(|| cli_error("r9p mount ensure requires -- before mount arguments"))?;
    let supervisor_args = args[..separator].to_vec();
    let mount_args = args[separator + 1..].to_vec();
    if mount_args.is_empty() {
        return Err(cli_error(
            "r9p mount ensure requires mount arguments after --",
        ));
    }
    let config = parse_mount_supervisor_config(supervisor_args)?;
    if config.unit.is_none() {
        return Err(cli_error("r9p mount ensure requires --unit"));
    }
    Ok((config, mount_args))
}

fn parse_mount_supervisor_config(args: Vec<String>) -> CliResult<MountSupervisorConfig> {
    let mut config = MountSupervisorConfig {
        mountpoint: PathBuf::new(),
        unit: None,
        expected_endpoint: None,
        expected_status_file: None,
        expected_change_feed: None,
        status_file: None,
        attempts: 16,
    };
    let mut index = 0_usize;
    while index < args.len() {
        match args[index].as_str() {
            "--mountpoint" => {
                index += 1;
                config.mountpoint = PathBuf::from(
                    args.get(index)
                        .ok_or_else(|| cli_error("missing mountpoint"))?,
                );
            }
            "--unit" => {
                index += 1;
                config.unit = Some(
                    args.get(index)
                        .ok_or_else(|| cli_error("missing unit"))?
                        .clone(),
                );
            }
            "--expect-endpoint" => {
                index += 1;
                config.expected_endpoint = Some(
                    args.get(index)
                        .ok_or_else(|| cli_error("missing expected endpoint"))?
                        .clone(),
                );
            }
            "--expect-status-file" => {
                index += 1;
                config.expected_status_file = Some(
                    args.get(index)
                        .ok_or_else(|| cli_error("missing expected status file"))?
                        .clone(),
                );
            }
            "--expect-change-feed" => {
                index += 1;
                config.expected_change_feed = Some(
                    args.get(index)
                        .ok_or_else(|| cli_error("missing expected change feed"))?
                        .clone(),
                );
            }
            "--status-file" => {
                index += 1;
                config.status_file = Some(PathBuf::from(
                    args.get(index)
                        .ok_or_else(|| cli_error("missing status file"))?,
                ));
            }
            "--attempts" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| cli_error("missing attempts"))?;
                config.attempts = value
                    .parse::<usize>()
                    .map_err(|_| cli_error(format!("invalid attempts {value}")))?;
            }
            "-h" | "--help" => mount_supervisor_usage(0),
            arg => return Err(cli_error(format!("unknown mount supervisor option {arg}"))),
        }
        index += 1;
    }
    if config.mountpoint.as_os_str().is_empty() {
        return Err(cli_error("missing --mountpoint"));
    }
    config.mountpoint = absolute_mountpoint(&config.mountpoint)?;
    Ok(config)
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
        connect_timeout: Duration::from_secs(30),
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
        change_feed_cursor_template: None,
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

fn assert_single_mount_layer(mountpoint: &Path) -> CliResult<()> {
    let targets = mounted_targets(mountpoint)?;
    match targets.len() {
        1 => Ok(()),
        0 => Err(cli_error(format!(
            "r9p_mount_absent:{}",
            mountpoint.display()
        ))),
        count => Err(cli_error(format!(
            "r9p_mount_stacked_layers:{}:{count}",
            mountpoint.display()
        ))),
    }
}

fn mounted_targets(mountpoint: &Path) -> CliResult<Vec<String>> {
    let absolute = absolute_mountpoint(mountpoint)?;
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo")
        .map_err(|error| cli_error(format!("read mountinfo: {error}")))?;
    Ok(mountinfo_targets_for_absolute(
        &mountinfo,
        absolute
            .to_str()
            .ok_or_else(|| cli_error("mountpoint is not valid UTF-8"))?,
    ))
}

fn absolute_mountpoint(mountpoint: &Path) -> CliResult<PathBuf> {
    if mountpoint.is_absolute() {
        return Ok(mountpoint.to_path_buf());
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(mountpoint))
        .map_err(|error| cli_error(format!("resolve mountpoint path: {error}")))
}

fn mountinfo_targets_for_absolute(mountinfo: &str, absolute_mountpoint: &str) -> Vec<String> {
    mountinfo
        .lines()
        .filter_map(mountinfo_target)
        .filter(|target| target == absolute_mountpoint)
        .collect()
}

fn mountinfo_target(line: &str) -> Option<String> {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 5 {
        return None;
    }
    Some(decode_mountinfo_path(fields[4]))
}

fn decode_mountinfo_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut bytes = path.as_bytes().iter().copied().peekable();
    while let Some(byte) = bytes.next() {
        if byte != b'\\' {
            out.push(byte as char);
            continue;
        }
        let mut octal = [0_u8; 3];
        let mut complete = true;
        for digit in &mut octal {
            match bytes.next() {
                Some(value @ b'0'..=b'7') => *digit = value,
                Some(value) => {
                    out.push('\\');
                    out.push(value as char);
                    complete = false;
                    break;
                }
                None => {
                    out.push('\\');
                    complete = false;
                    break;
                }
            }
        }
        if complete {
            let value = (octal[0] - b'0') * 64 + (octal[1] - b'0') * 8 + (octal[2] - b'0');
            out.push(value as char);
        }
    }
    out
}

fn systemd_unit_command(unit: &str) -> CliResult<String> {
    let output = Command::new("systemctl")
        .args([
            "--user",
            "show",
            unit,
            "-p",
            "ExecStart",
            "--value",
            "--no-pager",
        ])
        .output()
        .map_err(|error| cli_error(format!("systemctl show {unit}: {error}")))?;
    if !output.status.success() {
        return Err(cli_error(format!(
            "systemctl_show_failed:{unit}:{}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn assert_unit_command_contains(
    unit_command: &str,
    expected: Option<&str>,
    label: &str,
) -> CliResult<()> {
    let Some(expected) = expected else {
        return Ok(());
    };
    if unit_command.contains(expected) {
        return Ok(());
    }
    Err(cli_error(format!(
        "r9p_mount_missing_{label}:expected={expected}:unit={unit_command:?}"
    )))
}

fn unmount_mountpoint_layers(mountpoint: &Path, attempts: usize) -> CliResult<()> {
    for attempt in 0..=attempts {
        let targets = mounted_targets(mountpoint)?;
        if targets.is_empty() {
            return Ok(());
        }
        if attempt >= attempts {
            return Err(cli_error(format!(
                "r9p_mount_unmount_still_mounted:{}:{targets:?}",
                mountpoint.display()
            )));
        }
        lazy_unmount(mountpoint);
        thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn lazy_unmount(mountpoint: &Path) {
    if umount2_lazy(mountpoint) {
        return;
    }
    for (binary, args) in [
        ("fusermount3", &["-u", "-z"][..]),
        ("fusermount", &["-u", "-z"][..]),
        ("umount", &["-l"][..]),
    ] {
        if run_unmount_command(binary, args, mountpoint) {
            return;
        }
    }
}

fn umount2_lazy(mountpoint: &Path) -> bool {
    let Some(mountpoint) = mountpoint.to_str() else {
        return false;
    };
    let Ok(mountpoint) = CString::new(mountpoint) else {
        return false;
    };
    unsafe { libc::umount2(mountpoint.as_ptr(), libc::MNT_DETACH) == 0 }
}

fn run_unmount_command(binary: &str, args: &[&str], mountpoint: &Path) -> bool {
    let mut command = Command::new(binary);
    command
        .args(args)
        .arg(mountpoint)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let Ok(mut child) = command.spawn() else {
        return false;
    };
    wait_with_timeout(&mut child, Duration::from_secs(2)).unwrap_or(false)
}

fn wait_with_timeout(child: &mut std::process::Child, timeout: Duration) -> std::io::Result<bool> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.success());
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn mount_usage(code: i32) -> ! {
    eprintln!(
        "usage: r9p mount [--aname aname] [--uname uname] [--msize msize] [--attr-timeout seconds] [--entry-timeout seconds] [--request-timeout seconds] [--connect-timeout seconds] [--lookup-timeout seconds] [--read-timeout seconds] [--write-timeout seconds] [--mutation-timeout seconds] [--control-timeout seconds] [--interrupt-timeout seconds] [--max-workers count] [--max-background count] [--congestion-threshold count] [--diagnostics-file path] [--diagnostics-capacity count] [--status-file path] [--change-feed namespace-path] [--change-feed-cursor-template path-with-{{event_id}}] [--change-feed-scope scope] [--change-feed-poll-interval seconds] [--change-feed-backpressure count] endpoint mountpoint"
    );
    std::process::exit(code);
}

fn mount_supervisor_usage(code: i32) -> ! {
    eprintln!(
        "usage: r9p mount ensure|status|stop --mountpoint path [--unit name] [--status-file path] [--expect-endpoint endpoint] [--expect-change-feed path] [--expect-status-file path] [--attempts count] [-- mount args...]"
    );
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use super::{
        decode_mountinfo_path, mountinfo_targets_for_absolute, parse_mount_config,
        parse_mount_ensure_config, parse_mount_supervisor_config,
    };
    use crate::{target::Config, DEFAULT_MSIZE};

    fn global() -> Config {
        Config {
            address: None,
            aname: String::new(),
            uname: "codex".to_string(),
            msize: DEFAULT_MSIZE,
            msize_set: false,
            machine: false,
            request_timeout: Some(Duration::from_secs(30)),
            control_timeout: Some(Duration::from_secs(600)),
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
                "--connect-timeout".to_string(),
                "12".to_string(),
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
                "/feeds/namespace".to_string(),
                "--change-feed-cursor-template".to_string(),
                "/feeds/namespace-after/{event_id}".to_string(),
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
        assert_eq!(config.change_feed_path.as_deref(), Some("/feeds/namespace"));
        assert_eq!(
            config.change_feed_cursor_template.as_deref(),
            Some("/feeds/namespace-after/{event_id}")
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
        assert_eq!(config.connect_timeout, Duration::from_secs(12));
    }

    #[test]
    fn rejects_cursor_template_without_event_placeholder() {
        let result = parse_mount_config(
            global(),
            vec![
                "--change-feed".to_string(),
                "/feeds/namespace".to_string(),
                "--change-feed-cursor-template".to_string(),
                "/feeds/namespace-after/latest".to_string(),
                "127.0.0.1:564".to_string(),
                "/tmp/r9p-mount".to_string(),
            ],
        );

        assert!(result.is_err());
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
        assert_eq!(config.connect_timeout, Duration::from_secs(30));
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

    #[test]
    fn parses_mount_supervisor_options() {
        let config = parse_mount_supervisor_config(vec![
            "--mountpoint".to_string(),
            ".vault/live".to_string(),
            "--unit".to_string(),
            "vault-runtime-r9p-live-mount".to_string(),
            "--expect-endpoint".to_string(),
            "192.168.0.30:9564".to_string(),
            "--expect-change-feed".to_string(),
            "/feeds/namespace".to_string(),
            "--expect-status-file".to_string(),
            ".vault/live.status.json".to_string(),
            "--status-file".to_string(),
            ".vault/live.status.json".to_string(),
            "--attempts".to_string(),
            "3".to_string(),
        ])
        .expect("supervisor options should parse");
        let cwd = std::env::current_dir().expect("current dir");

        assert_eq!(config.mountpoint, cwd.join(".vault/live"));
        assert_eq!(config.unit.as_deref(), Some("vault-runtime-r9p-live-mount"));
        assert_eq!(
            config.expected_endpoint.as_deref(),
            Some("192.168.0.30:9564")
        );
        assert_eq!(
            config.expected_change_feed.as_deref(),
            Some("/feeds/namespace")
        );
        assert_eq!(
            config.expected_status_file.as_deref(),
            Some(".vault/live.status.json")
        );
        assert_eq!(
            config.status_file.as_deref(),
            Some(Path::new(".vault/live.status.json"))
        );
        assert_eq!(config.attempts, 3);
    }

    #[test]
    fn parses_mount_ensure_options_and_mount_invocation() {
        let (config, mount_args) = parse_mount_ensure_config(vec![
            "--mountpoint".to_string(),
            ".vault/live".to_string(),
            "--unit".to_string(),
            "vault-runtime-r9p-live-mount".to_string(),
            "--attempts".to_string(),
            "4".to_string(),
            "--".to_string(),
            "--uname".to_string(),
            "codex".to_string(),
            "192.168.0.30:9564".to_string(),
            ".vault/live".to_string(),
        ])
        .expect("ensure options should parse");
        let cwd = std::env::current_dir().expect("current dir");

        assert_eq!(config.mountpoint, cwd.join(".vault/live"));
        assert_eq!(config.unit.as_deref(), Some("vault-runtime-r9p-live-mount"));
        assert_eq!(config.attempts, 4);
        assert_eq!(
            mount_args,
            vec![
                "--uname".to_string(),
                "codex".to_string(),
                "192.168.0.30:9564".to_string(),
                ".vault/live".to_string()
            ]
        );
    }

    #[test]
    fn parses_mountinfo_targets_for_absolute_mountpoint() {
        let mountinfo = concat!(
            "42 28 0:37 / /sys/fs/fuse/connections rw - fusectl fusectl rw\n",
            "68 30 0:57 / /home/mrvamp/Vault/.vault/live rw - fuse /dev/fuse rw,user_id=1000\n",
            "69 30 0:58 / /home/mrvamp/Vault/.vault/live rw - fuse /dev/fuse rw,user_id=1000\n",
        );

        assert_eq!(
            mountinfo_targets_for_absolute(mountinfo, "/home/mrvamp/Vault/.vault/live"),
            vec![
                "/home/mrvamp/Vault/.vault/live".to_string(),
                "/home/mrvamp/Vault/.vault/live".to_string()
            ]
        );
    }

    #[test]
    fn decodes_mountinfo_octal_escapes() {
        assert_eq!(
            "/tmp/r9p mount/live",
            decode_mountinfo_path("/tmp/r9p\\040mount/live")
        );
    }
}
