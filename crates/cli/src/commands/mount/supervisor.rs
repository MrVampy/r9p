mod read_ahead;

use std::env;
use std::ffi::CString;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::errors::{cli_error, CliResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MountSupervisorConfig {
    pub(super) mountpoint: PathBuf,
    pub(super) unit: Option<String>,
    pub(super) unit_scope: Option<SystemdUnitScope>,
    pub(super) expected_endpoint: Option<String>,
    pub(super) expected_status_file: Option<String>,
    pub(super) expected_change_feed: Option<String>,
    pub(super) status_file: Option<PathBuf>,
    pub(super) attempts: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SystemdUnitScope {
    User,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MountReplacementConfig {
    pub(super) mountpoint: PathBuf,
    pub(super) unit: String,
    pub(super) unit_scope: SystemdUnitScope,
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

pub(super) fn mount_read_ahead_cmd(args: Vec<String>) -> CliResult<()> {
    read_ahead::run(args)
}

pub(super) fn mount_replace_cmd(
    config: MountReplacementConfig,
    mount: fuse::Config,
) -> CliResult<()> {
    assert_single_mount_layer(&config.mountpoint)?;
    let active_pid = systemd_main_pid(&config.unit, config.unit_scope)?;
    let (mut prepared_parent, prepared_child) = UnixStream::pair()
        .map_err(|error| cli_error(format!("create replacement preflight channel: {error}")))?;
    let (mut start_parent, start_child) = UnixStream::pair()
        .map_err(|error| cli_error(format!("create replacement start channel: {error}")))?;
    let (mut ready_parent, ready_child) = UnixStream::pair()
        .map_err(|error| cli_error(format!("create replacement readiness channel: {error}")))?;
    for channel in [&prepared_parent, &start_parent, &ready_parent] {
        channel
            .set_read_timeout(Some(Duration::from_secs(60)))
            .map_err(|error| cli_error(format!("set replacement channel timeout: {error}")))?;
        channel
            .set_write_timeout(Some(Duration::from_secs(60)))
            .map_err(|error| cli_error(format!("set replacement channel timeout: {error}")))?;
    }
    let replacement_pid = unsafe { libc::fork() };
    if replacement_pid < 0 {
        return Err(cli_error(format!(
            "fork replacement mount: {}",
            std::io::Error::last_os_error()
        )));
    }
    if replacement_pid == 0 {
        drop(prepared_parent);
        drop(start_parent);
        drop(ready_parent);
        let exit_code =
            match fuse::mount_replacement(mount, prepared_child, start_child, ready_child) {
                Ok(()) => 0,
                Err(error) => {
                    eprintln!("r9p replacement mount: {}", error.message());
                    1
                }
            };
        unsafe {
            libc::_exit(exit_code);
        }
    }
    drop(prepared_child);
    drop(start_child);
    drop(ready_child);

    let result = replace_mount_generation(
        &config,
        active_pid,
        replacement_pid,
        &mut prepared_parent,
        &mut start_parent,
        &mut ready_parent,
    );
    if result.is_err() {
        terminate_replacement(replacement_pid);
    }
    result
}

fn replace_mount_generation(
    config: &MountReplacementConfig,
    active_pid: libc::pid_t,
    replacement_pid: libc::pid_t,
    prepared: &mut UnixStream,
    start: &mut UnixStream,
    ready: &mut UnixStream,
) -> CliResult<()> {
    expect_replacement_event(prepared, b'P', "preflight")?;
    signal_process(active_pid, libc::SIGHUP, "retire active mount")?;
    wait_for_mount_absent(&config.mountpoint, config.attempts)?;
    start
        .write_all(b"G")
        .map_err(|error| cli_error(format!("start replacement mount: {error}")))?;
    expect_replacement_event(ready, b'R', "readiness")?;
    wait_for_main_pid(
        &config.unit,
        config.unit_scope,
        replacement_pid,
        config.attempts,
    )?;
    signal_process(
        active_pid,
        libc::SIGUSR1,
        "release retired mount generation",
    )?;
    println!(
        "mountpoint {} replaced process {} -> {}",
        config.mountpoint.display(),
        active_pid,
        replacement_pid
    );
    Ok(())
}

fn expect_replacement_event(channel: &mut UnixStream, expected: u8, label: &str) -> CliResult<()> {
    let mut event = [0_u8; 1];
    channel
        .read_exact(&mut event)
        .map_err(|error| cli_error(format!("wait for replacement mount {label}: {error}")))?;
    if event[0] != expected {
        return Err(cli_error(format!(
            "replacement_mount_{label}_failed:event={}",
            event[0]
        )));
    }
    Ok(())
}

fn signal_process(pid: libc::pid_t, signal: libc::c_int, label: &str) -> CliResult<()> {
    if unsafe { libc::kill(pid, signal) } == 0 {
        return Ok(());
    }
    Err(cli_error(format!(
        "{label}: pid={pid}: {}",
        std::io::Error::last_os_error()
    )))
}

fn wait_for_mount_absent(mountpoint: &Path, attempts: usize) -> CliResult<()> {
    for attempt in 0..=attempts {
        if mounted_targets(mountpoint)?.is_empty() {
            return Ok(());
        }
        if attempt >= attempts {
            return Err(cli_error(format!(
                "replacement_mount_detach_timeout:{}",
                mountpoint.display()
            )));
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn wait_for_main_pid(
    unit: &str,
    scope: SystemdUnitScope,
    expected_pid: libc::pid_t,
    attempts: usize,
) -> CliResult<()> {
    for attempt in 0..=attempts {
        let output = systemd_command("systemctl", scope)
            .args(["show", unit, "-p", "MainPID", "--value", "--no-pager"])
            .output()
            .map_err(|error| cli_error(format!("systemctl show {unit}: {error}")))?;
        let actual = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if output.status.success() && actual == expected_pid.to_string() {
            return Ok(());
        }
        if attempt >= attempts {
            return Err(cli_error(format!(
                "replacement_mount_main_pid_not_adopted:unit={unit}:expected={expected_pid}:actual={actual}"
            )));
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn systemd_main_pid(unit: &str, scope: SystemdUnitScope) -> CliResult<libc::pid_t> {
    let output = systemd_command("systemctl", scope)
        .args(["show", unit, "-p", "MainPID", "--value", "--no-pager"])
        .output()
        .map_err(|error| cli_error(format!("systemctl show {unit}: {error}")))?;
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let pid = value
        .parse::<libc::pid_t>()
        .map_err(|_| cli_error(format!("invalid main pid for {unit}: {value}")))?;
    if !output.status.success() || pid <= 1 {
        return Err(cli_error(format!(
            "replacement_mount_main_pid_unavailable:unit={unit}:pid={value}"
        )));
    }
    Ok(pid)
}

fn terminate_replacement(pid: libc::pid_t) {
    unsafe {
        libc::kill(pid, libc::SIGKILL);
        libc::waitpid(pid, std::ptr::null_mut(), 0);
    }
}

fn check_mount_status(config: &MountSupervisorConfig) -> CliResult<()> {
    assert_single_mount_layer(&config.mountpoint)?;
    if let (Some(unit), Some(scope)) = (&config.unit, config.unit_scope) {
        let unit_command = systemd_unit_command(unit, scope)?;
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
    if let (Some(unit), Some(scope)) = (&config.unit, config.unit_scope) {
        let _ = systemd_command("systemctl", scope)
            .args(["stop", unit])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    unmount_mountpoint_layers(&config.mountpoint, config.attempts)?;
    if let (Some(unit), Some(scope)) = (&config.unit, config.unit_scope) {
        let _ = systemd_command("systemctl", scope)
            .args(["reset-failed", unit])
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
    let scope = config
        .unit_scope
        .ok_or_else(|| cli_error("r9p mount ensure requires --unit-scope"))?;
    let executable =
        env::current_exe().map_err(|error| cli_error(format!("resolve current r9p: {error}")))?;
    let mut command = systemd_command("systemd-run", scope);
    command.args([
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

pub(super) fn parse_mount_replacement_config(
    args: Vec<String>,
) -> CliResult<(MountReplacementConfig, Vec<String>)> {
    let separator = args
        .iter()
        .position(|arg| arg == "--")
        .ok_or_else(|| cli_error("r9p mount replace requires -- before mount arguments"))?;
    let replacement_args = &args[..separator];
    let mount_args = args[separator + 1..].to_vec();
    if mount_args.is_empty() {
        return Err(cli_error(
            "r9p mount replace requires mount arguments after --",
        ));
    }
    let mut mountpoint = PathBuf::new();
    let mut unit = None;
    let mut unit_scope = None;
    let mut attempts = 100_usize;
    let mut index = 0_usize;
    while index < replacement_args.len() {
        match replacement_args[index].as_str() {
            "--mountpoint" => {
                index += 1;
                mountpoint = PathBuf::from(
                    replacement_args
                        .get(index)
                        .ok_or_else(|| cli_error("missing replacement mountpoint"))?,
                );
            }
            "--unit" => {
                index += 1;
                unit = Some(
                    replacement_args
                        .get(index)
                        .ok_or_else(|| cli_error("missing replacement unit"))?
                        .clone(),
                );
            }
            "--unit-scope" => {
                index += 1;
                unit_scope = Some(parse_systemd_unit_scope(
                    replacement_args
                        .get(index)
                        .ok_or_else(|| cli_error("missing replacement unit scope"))?,
                )?);
            }
            "--attempts" => {
                index += 1;
                let value = replacement_args
                    .get(index)
                    .ok_or_else(|| cli_error("missing replacement attempts"))?;
                attempts = value
                    .parse::<usize>()
                    .map_err(|_| cli_error(format!("invalid replacement attempts {value}")))?;
            }
            "-h" | "--help" => mount_supervisor_usage(0),
            option => {
                return Err(cli_error(format!(
                    "unknown mount replacement option {option}"
                )))
            }
        }
        index += 1;
    }
    if mountpoint.as_os_str().is_empty() {
        return Err(cli_error("missing replacement --mountpoint"));
    }
    let config = MountReplacementConfig {
        mountpoint: absolute_mountpoint(&mountpoint)?,
        unit: unit.ok_or_else(|| cli_error("missing replacement --unit"))?,
        unit_scope: unit_scope
            .ok_or_else(|| cli_error("missing replacement --unit-scope user|system"))?,
        attempts,
    };
    Ok((config, mount_args))
}

pub(super) fn parse_mount_supervisor_config(args: Vec<String>) -> CliResult<MountSupervisorConfig> {
    let mut config = MountSupervisorConfig {
        mountpoint: PathBuf::new(),
        unit: None,
        unit_scope: None,
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
            "--unit-scope" => {
                index += 1;
                config.unit_scope = Some(parse_systemd_unit_scope(
                    args.get(index)
                        .ok_or_else(|| cli_error("missing unit scope"))?,
                )?);
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
    match (&config.unit, config.unit_scope) {
        (Some(_), None) => return Err(cli_error("--unit requires --unit-scope user|system")),
        (None, Some(_)) => return Err(cli_error("--unit-scope requires --unit")),
        _ => {}
    }
    config.mountpoint = absolute_mountpoint(&config.mountpoint)?;
    Ok(config)
}

fn parse_systemd_unit_scope(value: &str) -> CliResult<SystemdUnitScope> {
    match value {
        "user" => Ok(SystemdUnitScope::User),
        "system" => Ok(SystemdUnitScope::System),
        _ => Err(cli_error(format!(
            "invalid unit scope {value}; expected user or system"
        ))),
    }
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

fn systemd_unit_command(unit: &str, scope: SystemdUnitScope) -> CliResult<String> {
    let output = systemd_command("systemctl", scope)
        .args(["show", unit, "-p", "ExecStart", "--value", "--no-pager"])
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

pub(super) fn systemd_command(program: &str, scope: SystemdUnitScope) -> Command {
    let mut command = Command::new(program);
    if scope == SystemdUnitScope::User {
        command.arg("--user");
    }
    command
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
        "usage: r9p mount ensure|status|stop --mountpoint path [--unit name --unit-scope user|system] [--status-file path] [--expect-endpoint endpoint] [--expect-change-feed path] [--expect-status-file path] [--attempts count] [-- mount args...]\n       r9p mount replace --mountpoint path --unit name --unit-scope user|system [--attempts count] -- mount args...\n       r9p mount read-ahead --mountpoint path --kilobytes count [--attempts count]"
    );
    std::process::exit(code);
}
