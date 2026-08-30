use std::{
    env, fs, io,
    path::{Path, PathBuf},
    process::Child,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

#[test]
#[ignore = "live-gated: set R9P_PLAN83_ENDPOINT and optionally R9P_PLAN83_FUSE_MOUNT"]
fn plan83_live_latency_matrix() -> TestResult<()> {
    let Some(endpoint) = env::var("R9P_PLAN83_ENDPOINT")
        .ok()
        .filter(|value| !value.is_empty())
    else {
        eprintln!("skipping: R9P_PLAN83_ENDPOINT is not set");
        return Ok(());
    };
    let fuse_mount = env::var("R9P_PLAN83_FUSE_MOUNT")
        .ok()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);

    let socket = temp_path("r9p-plan83-live-session.sock");
    let socket_arg = socket.to_string_lossy().into_owned();
    let mut session = ChildGuard::spawn(
        Command::new(r9p_bin())
            .arg("-a")
            .arg(&endpoint)
            .arg("-u")
            .arg("codex")
            .arg("-A")
            .arg("/")
            .arg("-m")
            .arg("65536")
            .arg("session")
            .arg("serve")
            .arg("--socket")
            .arg(&socket_arg)
            .arg("--change-feed")
            .arg("/events/namespace/recent")
            .arg("--change-feed-stream")
            .arg("/events/namespace/stream")
            .arg("--change-feed-cursor-template")
            .arg("/events/namespace/since/{event_id}")
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )?;
    wait_for_session_status(&socket)?;

    let raw_root = measure_command("raw_cli_ls_root", &mut r9p_command(&endpoint, &["ls", "/"]))?;
    print_sample(&raw_root);
    let raw_srv = measure_command(
        "raw_cli_ls_srv",
        &mut r9p_command(&endpoint, &["ls", "/srv"]),
    )?;
    print_sample(&raw_srv);
    let raw_status = measure_command(
        "raw_cli_read_status",
        &mut r9p_command(&endpoint, &["read", "/status"]),
    )?;
    print_sample(&raw_status);

    let session_status = measure_command(
        "session_status",
        Command::new(r9p_bin())
            .arg("session")
            .arg("status")
            .arg("--socket")
            .arg(&socket_arg),
    )?;
    print_sample(&session_status);

    let session_control_rpc = measure_command(
        "session_control_rpc_stat_status",
        Command::new(r9p_bin())
            .arg("-a")
            .arg(format!("unix!{socket_arg}"))
            .arg("rpc")
            .arg("/query")
            .arg(r#"{"op":"stat","path":"/status"}"#),
    )?;
    print_sample(&session_control_rpc);

    let session_list_srv_cold = measure_command(
        "session_list_srv_cold",
        Command::new(r9p_bin())
            .arg("session")
            .arg("list")
            .arg("--socket")
            .arg(&socket_arg)
            .arg("/srv"),
    )?;
    print_sample(&session_list_srv_cold);
    let session_list_srv_warm = measure_command(
        "session_list_srv_warm",
        Command::new(r9p_bin())
            .arg("session")
            .arg("list")
            .arg("--socket")
            .arg(&socket_arg)
            .arg("/srv"),
    )?;
    print_sample(&session_list_srv_warm);

    let session_snapshot_srv_cold = measure_command(
        "session_snapshot_srv_depth2_cold",
        Command::new(r9p_bin())
            .arg("session")
            .arg("snapshot")
            .arg("--socket")
            .arg(&socket_arg)
            .arg("--depth")
            .arg("2")
            .arg("/srv"),
    )?;
    print_sample(&session_snapshot_srv_cold);
    let session_snapshot_srv_warm = measure_command(
        "session_snapshot_srv_depth2_warm",
        Command::new(r9p_bin())
            .arg("session")
            .arg("snapshot")
            .arg("--socket")
            .arg(&socket_arg)
            .arg("--depth")
            .arg("2")
            .arg("/srv"),
    )?;
    print_sample(&session_snapshot_srv_warm);
    assert_contains(&session_snapshot_srv_warm.output, "\"stat_misses\":0")?;
    assert_contains(&session_snapshot_srv_warm.output, "\"dir_misses\":0")?;

    let session_read_status = measure_command(
        "session_read_status",
        Command::new(r9p_bin())
            .arg("session")
            .arg("read")
            .arg("--socket")
            .arg(&socket_arg)
            .arg("/status"),
    )?;
    print_sample(&session_read_status);

    if let Some(mount) = fuse_mount.as_deref() {
        if mount.exists() {
            print_sample(&measure_fs_read_dir("fuse_root_readdir", mount)?);
            print_sample(&measure_fs_read_dir("fuse_root_readdir_warm", mount)?);
            print_sample(&measure_fs_read_dir(
                "fuse_srv_readdir",
                &mount.join("srv"),
            )?);
            print_sample(&measure_fs_read_dir(
                "fuse_srv_readdir_warm",
                &mount.join("srv"),
            )?);
            print_sample(&measure_fs_read_file(
                "fuse_read_status",
                &mount.join("status"),
            )?);
        } else {
            eprintln!("skipping FUSE samples: {} does not exist", mount.display());
        }
    }

    session.wait_or_kill()?;
    let _ = fs::remove_file(&socket);
    let raw_after_stop = measure_command(
        "raw_cli_ls_srv_after_session_stop",
        &mut r9p_command(&endpoint, &["ls", "/srv"]),
    )?;
    print_sample(&raw_after_stop);
    Ok(())
}

struct Sample {
    label: &'static str,
    elapsed: Duration,
    output: Vec<u8>,
}

fn r9p_command(endpoint: &str, args: &[&str]) -> Command {
    let mut command = Command::new(r9p_bin());
    command
        .arg("-a")
        .arg(endpoint)
        .arg("-u")
        .arg("codex")
        .arg("-A")
        .arg("/");
    command.args(args);
    command
}

fn measure_command(label: &'static str, command: &mut Command) -> TestResult<Sample> {
    let started = Instant::now();
    let output = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    let elapsed = started.elapsed();
    if !output.status.success() {
        return Err(test_error(format!(
            "{label} failed stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(Sample {
        label,
        elapsed,
        output: output.stdout,
    })
}

fn measure_fs_read_dir(label: &'static str, path: &Path) -> TestResult<Sample> {
    let started = Instant::now();
    let mut names = Vec::new();
    for entry in fs::read_dir(path)? {
        names.push(entry?.file_name());
    }
    let elapsed = started.elapsed();
    Ok(Sample {
        label,
        elapsed,
        output: names
            .iter()
            .map(|name| name.to_string_lossy())
            .collect::<Vec<_>>()
            .join("\n")
            .into_bytes(),
    })
}

fn measure_fs_read_file(label: &'static str, path: &Path) -> TestResult<Sample> {
    let started = Instant::now();
    let output = fs::read(path)?;
    Ok(Sample {
        label,
        elapsed: started.elapsed(),
        output,
    })
}

fn print_sample(sample: &Sample) {
    println!(
        "plan83_latency\tlabel={}\tseconds={:.6}\tbytes={}",
        sample.label,
        sample.elapsed.as_secs_f64(),
        sample.output.len()
    );
}

fn wait_for_session_status(socket: &Path) -> TestResult<()> {
    let socket_arg = socket.to_string_lossy().into_owned();
    let started = Instant::now();
    loop {
        let output = Command::new(r9p_bin())
            .arg("session")
            .arg("status")
            .arg("--socket")
            .arg(&socket_arg)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();
        if let Ok(output) = output {
            if output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .contains("\"kind\":\"session.status.v1\"")
            {
                return Ok(());
            }
        }
        if started.elapsed() > Duration::from_secs(5) {
            return Err(test_error("session status did not become readable"));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn assert_contains(output: &[u8], needle: &str) -> TestResult<()> {
    if String::from_utf8_lossy(output).contains(needle) {
        Ok(())
    } else {
        Err(test_error(format!(
            "output did not contain {needle}: {}",
            String::from_utf8_lossy(output)
        )))
    }
}

fn test_error(message: impl Into<String>) -> Box<dyn std::error::Error> {
    Box::new(io::Error::other(message.into()))
}

fn r9p_bin() -> PathBuf {
    env::var_os("CARGO_BIN_EXE_r9p-mount")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/debug/r9p-mount"))
}

fn temp_path(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    env::temp_dir().join(format!("{label}-{}-{nanos}", std::process::id()))
}

struct ChildGuard {
    child: Option<Child>,
}

impl ChildGuard {
    fn spawn(command: &mut Command) -> io::Result<Self> {
        command.spawn().map(|child| Self { child: Some(child) })
    }

    fn kill(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
        }
    }

    fn wait_or_kill(&mut self) -> io::Result<()> {
        if let Some(mut child) = self.child.take() {
            if child.try_wait()?.is_none() {
                let _ = child.kill();
            }
            let _ = child.wait()?;
        }
        Ok(())
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.kill();
        let _ = self.wait_or_kill();
    }
}
