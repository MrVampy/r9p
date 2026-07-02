mod support;

use std::{
    fs, io,
    io::Write,
    process::{Command, Stdio},
};

use support::{
    fuse_host::{
        host_can_run_fuse, r9p_bin, read_dir_names, temp_path, unique_temp_dir, unmount,
        wait_for_dir_entry, wait_for_mounted_file, wait_for_status_event, ChildGuard, TestResult,
    },
    stream_namespace::NamespaceServer,
};

#[test]
#[ignore = "host-gated: requires /dev/fuse, fusermount, and user mount permission"]
fn fuse_stream_event_invalidates_cached_directory_after_9p_mutation() -> TestResult<()> {
    if !host_can_run_fuse() {
        return Ok(());
    }

    let server = NamespaceServer::start()?;
    let mountpoint = unique_temp_dir("r9p-fuse-stream-mount")?;
    let status = temp_path("r9p-fuse-stream-status.json");
    let diagnostics = temp_path("r9p-fuse-stream-diagnostics.jsonl");

    let mut mount = ChildGuard::spawn(
        Command::new(r9p_bin())
            .arg("mount")
            .arg("--request-timeout")
            .arg("2")
            .arg("--lookup-timeout")
            .arg("2")
            .arg("--read-timeout")
            .arg("5")
            .arg("--control-timeout")
            .arg("2")
            .arg("--attr-timeout")
            .arg("60")
            .arg("--entry-timeout")
            .arg("60")
            .arg("--status-file")
            .arg(&status)
            .arg("--diagnostics-file")
            .arg(&diagnostics)
            .arg("--change-feed")
            .arg("/events/namespace/recent")
            .arg("--change-feed-stream")
            .arg("/events/namespace/stream")
            .arg("--change-feed-cursor-template")
            .arg("/events/namespace/since/{event_id}")
            .arg(&server.endpoint)
            .arg(&mountpoint)
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )?;

    wait_for_mounted_file(&mountpoint.join("watched.txt"), "watched\n")?;
    server.wait_for_stream_reader()?;

    let before = read_dir_names(&mountpoint)?;
    assert!(
        !before.iter().any(|name| name == "created.txt"),
        "created.txt should not be visible before mutation: {before:?}"
    );

    write_mutation(&server.endpoint)?;
    wait_for_status_event(&status, "event-1")?;
    wait_for_dir_entry(&mountpoint, "created.txt")?;
    wait_for_mounted_file(&mountpoint.join("created.txt"), "created\n")?;

    unmount(&mountpoint);
    mount.wait_or_kill()?;
    let _ = fs::remove_dir_all(&mountpoint);
    let _ = fs::remove_file(status);
    let _ = fs::remove_file(diagnostics);
    Ok(())
}

fn write_mutation(endpoint: &str) -> io::Result<()> {
    let mut child = Command::new(r9p_bin())
        .arg("-a")
        .arg(endpoint)
        .arg("write")
        .arg("/mutate")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .as_mut()
        .ok_or_else(|| io::Error::other("mutation stdin unavailable"))?
        .write_all(b"create\n")?;
    let output = child.wait_with_output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "mutation write failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}
