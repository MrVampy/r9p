mod support;

use std::{
    fs, io,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use support::{
    fuse_host::{
        host_can_run_fuse, r9p_bin, read_dir_names, temp_path, unique_temp_dir, unmount,
        wait_for_dir_entry, wait_for_mounted_file, wait_for_status_event, write_9p_file,
        ChildGuard, TestResult,
    },
    stream_namespace::NamespaceServer,
};

#[test]
#[ignore = "host-gated: requires /dev/fuse, fusermount, and user mount permission"]
fn session_hosted_fuse_uses_shared_feed_for_invalidation() -> TestResult<()> {
    if !host_can_run_fuse() {
        return Ok(());
    }

    let server = NamespaceServer::start()?;
    let socket = temp_path("r9p-session-hosted-fuse.sock");
    let socket_arg = socket.to_string_lossy().into_owned();
    let mountpoint = unique_temp_dir("r9p-session-hosted-fuse-mount")?;
    let status = temp_path("r9p-session-hosted-fuse-status.json");
    let diagnostics = temp_path("r9p-session-hosted-fuse-diagnostics.jsonl");

    let mut session = ChildGuard::spawn(
        Command::new(r9p_bin())
            .arg("-a")
            .arg(&server.endpoint)
            .arg("-u")
            .arg("test")
            .arg("-A")
            .arg("/")
            .arg("-m")
            .arg("65536")
            .arg("session")
            .arg("serve")
            .arg("--socket")
            .arg(&socket_arg)
            .arg("--mount")
            .arg(&mountpoint)
            .arg("--mount-status-file")
            .arg(&status)
            .arg("--mount-diagnostics-file")
            .arg(&diagnostics)
            .arg("--mount-attr-timeout")
            .arg("60")
            .arg("--mount-entry-timeout")
            .arg("60")
            .arg("--change-feed")
            .arg("/events/namespace/recent")
            .arg("--change-feed-stream")
            .arg("/events/namespace/stream")
            .arg("--change-feed-cursor-template")
            .arg("/events/namespace/since/{event_id}")
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )?;

    wait_for_mounted_file(&mountpoint.join("watched.txt"), "watched\n")?;
    wait_for_control_status(&socket, "\"kind\":\"session.status.v1\"")?;
    server.wait_for_stream_reader_count(1)?;

    let before = read_dir_names(&mountpoint)?;
    assert!(
        !before.iter().any(|name| name == "created.txt"),
        "created.txt should not be visible before mutation: {before:?}"
    );

    write_9p_file(&server.endpoint, "/mutate", b"create\n")?;
    wait_for_status_event(&status, "event-1")?;
    wait_for_control_status(&socket, "\"last_event_id\":\"event-1\"")?;
    server.wait_for_stream_reader_count(1)?;
    wait_for_dir_entry(&mountpoint, "created.txt")?;
    wait_for_mounted_file(&mountpoint.join("created.txt"), "created\n")?;

    unmount(&mountpoint);
    session.wait_or_kill()?;
    let _ = fs::remove_dir_all(&mountpoint);
    let _ = fs::remove_file(socket);
    let _ = fs::remove_file(status);
    let _ = fs::remove_file(diagnostics);
    Ok(())
}

fn wait_for_control_status(socket: &Path, needle: &str) -> io::Result<()> {
    let started = Instant::now();
    let address = format!("unix!{}", socket.display());
    loop {
        let output = Command::new(r9p_bin())
            .arg("-a")
            .arg(&address)
            .arg("read")
            .arg("/status")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();
        if let Ok(output) = output {
            if output.status.success() && String::from_utf8_lossy(&output.stdout).contains(needle) {
                return Ok(());
            }
        }
        if started.elapsed() > Duration::from_secs(5) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("session status did not contain {needle}"),
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
}
