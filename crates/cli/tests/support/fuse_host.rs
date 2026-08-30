use std::{
    error::Error,
    fs, io,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub type TestResult<T> = Result<T, Box<dyn Error>>;

pub fn host_can_run_fuse() -> bool {
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

pub fn r9p_bin() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_r9p")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/debug/r9p"))
}

pub fn unique_temp_dir(label: &str) -> io::Result<PathBuf> {
    let path = temp_path(label);
    fs::create_dir_all(&path)?;
    Ok(path)
}

pub fn temp_path(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("{label}-{}-{nanos}", std::process::id()))
}

pub fn read_dir_names(path: &Path) -> io::Result<Vec<String>> {
    let mut names = fs::read_dir(path)?
        .map(|entry| {
            entry.and_then(|entry| {
                entry
                    .file_name()
                    .into_string()
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-utf8 name"))
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    names.sort();
    Ok(names)
}

pub fn wait_for_dir_entry(path: &Path, name: &str) -> io::Result<()> {
    let started = Instant::now();
    loop {
        let names = read_dir_names(path)?;
        if names.iter().any(|entry| entry == name) {
            return Ok(());
        }
        if started.elapsed() > Duration::from_secs(5) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{name} did not appear in directory; last entries: {names:?}"),
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

pub fn wait_for_mounted_file(path: &Path, expected: &str) -> io::Result<()> {
    let started = Instant::now();
    loop {
        match fs::read_to_string(path) {
            Ok(contents) if contents == expected => return Ok(()),
            Ok(contents) if started.elapsed() > Duration::from_secs(5) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unexpected mounted file contents {contents:?}"),
                ));
            }
            Ok(_) => {}
            Err(error) if started.elapsed() > Duration::from_secs(5) => return Err(error),
            Err(_) => {}
        }
        thread::sleep(Duration::from_millis(20));
    }
}

pub fn wait_for_status_event(path: &Path, event_id: &str) -> io::Result<()> {
    let started = Instant::now();
    loop {
        if let Ok(status) = fs::read_to_string(path) {
            if status.contains("\"source\":\"stream\"") && status.contains(event_id) {
                return Ok(());
            }
        }
        if started.elapsed() > Duration::from_secs(5) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "stream event did not reach mount status",
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

pub fn unmount(path: &Path) {
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

pub fn write_9p_file(endpoint: &str, path: &str, data: &[u8]) -> io::Result<()> {
    let mut child = Command::new(r9p_bin())
        .arg("-a")
        .arg(endpoint)
        .arg("write")
        .arg(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .as_mut()
        .ok_or_else(|| io::Error::other("write stdin unavailable"))?
        .write_all(data)?;
    let output = child.wait_with_output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "9P write failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

pub struct ChildGuard {
    child: Option<Child>,
}

impl ChildGuard {
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        command.spawn().map(|child| Self { child: Some(child) })
    }

    fn kill(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
        }
    }

    pub fn wait_or_kill(&mut self) -> io::Result<()> {
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
