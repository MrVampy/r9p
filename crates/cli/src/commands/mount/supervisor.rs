use std::env;
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::errors::{cli_error, CliResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MountSupervisorConfig {
    pub(super) mountpoint: PathBuf,
    pub(super) unit: Option<String>,
    pub(super) expected_endpoint: Option<String>,
    pub(super) expected_status_file: Option<String>,
    pub(super) expected_change_feed: Option<String>,
    pub(super) status_file: Option<PathBuf>,
    pub(super) attempts: usize,
}

pub(super) fn mount_status_cmd(args: Vec<String>) -> CliResult<()> {
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

pub(super) fn mount_ensure_cmd(args: Vec<String>) -> CliResult<()> {
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

pub(super) fn mount_stop_cmd(args: Vec<String>) -> CliResult<()> {
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
    command.args([
        "--user",
        "--unit",
        unit,
        "--collect",
        "--same-dir",
        "--property=Restart=on-failure",
        "--property=RestartSec=2",
    ]);
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

pub(super) fn parse_mount_ensure_config(
    args: Vec<String>,
) -> CliResult<(MountSupervisorConfig, Vec<String>)> {
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

pub(super) fn parse_mount_supervisor_config(args: Vec<String>) -> CliResult<MountSupervisorConfig> {
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

pub(super) fn mountinfo_targets_for_absolute(
    mountinfo: &str,
    absolute_mountpoint: &str,
) -> Vec<String> {
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

pub(super) fn decode_mountinfo_path(path: &str) -> String {
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

fn mount_supervisor_usage(code: i32) -> ! {
    eprintln!(
        "usage: r9p mount ensure|status|stop --mountpoint path [--unit name] [--status-file path] [--expect-endpoint endpoint] [--expect-change-feed path] [--expect-status-file path] [--attempts count] [-- mount args...]"
    );
    std::process::exit(code);
}
