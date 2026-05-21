use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[test]
#[ignore = "host-gated: requires /dev/fuse, fusermount, and user mount permission"]
fn fuse_mount_handles_parallel_recursive_reads() -> io::Result<()> {
    if !host_can_run_fuse() {
        return Ok(());
    }

    let root = unique_temp_dir("r9p-fuse-export")?;
    let mountpoint = unique_temp_dir("r9p-fuse-mount")?;
    let descriptor = root.with_extension("desc");
    let diagnostics = root.with_extension("jsonl");
    seed_tree(&root)?;

    let mut export = ChildGuard::spawn(
        Command::new(r9p_bin())
            .arg("export")
            .arg("--bind")
            .arg("127.0.0.1:0")
            .arg("--descriptor-file")
            .arg(&descriptor)
            .arg(&root)
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )?;
    let endpoint = wait_for_descriptor_endpoint(&descriptor)?;

    let mut mount = ChildGuard::spawn(
        Command::new(r9p_bin())
            .arg("mount")
            .arg("--request-timeout")
            .arg("1")
            .arg("--lookup-timeout")
            .arg("1")
            .arg("--read-timeout")
            .arg("1")
            .arg("--max-workers")
            .arg("2")
            .arg("--max-background")
            .arg("2")
            .arg("--diagnostics-file")
            .arg(&diagnostics)
            .arg(endpoint)
            .arg(&mountpoint)
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )?;
    wait_for_mounted_file(&mountpoint.join("dir-0/file-0.txt"))?;

    let mut workers = Vec::new();
    for _ in 0..8 {
        let mountpoint = mountpoint.clone();
        workers.push(thread::spawn(move || -> io::Result<()> {
            for _ in 0..16 {
                read_tree(&mountpoint)?;
            }
            Ok(())
        }));
    }
    for worker in workers {
        worker
            .join()
            .map_err(|_| io::Error::other("stress worker panicked"))??;
    }

    unmount(&mountpoint);
    mount.wait_or_kill()?;
    export.kill();
    export.wait_or_kill()?;
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&mountpoint);
    let _ = fs::remove_file(descriptor);
    let _ = fs::remove_file(diagnostics);
    Ok(())
}

fn host_can_run_fuse() -> bool {
    Path::new("/dev/fuse").exists()
        && (Command::new("fusermount3")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
            || Command::new("fusermount")
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|status| status.success())
                .unwrap_or(false))
}

fn r9p_bin() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_r9p")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/debug/r9p"))
}

fn unique_temp_dir(label: &str) -> io::Result<PathBuf> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("{label}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&path)?;
    Ok(path)
}

fn seed_tree(root: &Path) -> io::Result<()> {
    for dir in 0..8 {
        let dir_path = root.join(format!("dir-{dir}"));
        fs::create_dir_all(&dir_path)?;
        for file in 0..8 {
            fs::write(
                dir_path.join(format!("file-{file}.txt")),
                format!("dir={dir} file={file}\n"),
            )?;
        }
    }
    Ok(())
}

fn wait_for_descriptor_endpoint(path: &Path) -> io::Result<String> {
    let started = Instant::now();
    loop {
        if let Ok(descriptor) = fs::read_to_string(path) {
            for line in descriptor.lines() {
                if let Some(endpoint) = line.strip_prefix("endpoint_bind\t") {
                    return Ok(endpoint.to_string());
                }
            }
        }
        if started.elapsed() > Duration::from_secs(5) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "descriptor endpoint did not appear",
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_mounted_file(path: &Path) -> io::Result<()> {
    let started = Instant::now();
    loop {
        match fs::read_to_string(path) {
            Ok(contents) if contents.contains("dir=0 file=0") => return Ok(()),
            Ok(_) => {}
            Err(error) if started.elapsed() > Duration::from_secs(5) => return Err(error),
            Err(_) => {}
        }
        if started.elapsed() > Duration::from_secs(5) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "mounted file did not become readable",
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn read_tree(root: &Path) -> io::Result<()> {
    for dir in fs::read_dir(root)? {
        let dir = dir?;
        if !dir.file_type()?.is_dir() {
            continue;
        }
        for file in fs::read_dir(dir.path())? {
            let file = file?;
            if file.file_type()?.is_file() {
                let contents = fs::read_to_string(file.path())?;
                if !contents.contains("dir=") {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "unexpected file contents",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn unmount(path: &Path) {
    for binary in ["fusermount3", "fusermount"] {
        if Command::new(binary)
            .arg("-u")
            .arg(path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
        {
            return;
        }
    }
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
