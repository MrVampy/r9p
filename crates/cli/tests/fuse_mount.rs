use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use r9p::{
    codec,
    fid::Fid,
    message::TMessage,
    qid::{Qid, DMDIR},
    server::{FileTree, OpenFile, ReadData, Server},
    stat::Stat,
    Result as R9pResult,
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

#[test]
#[ignore = "host-gated: requires /dev/fuse, fusermount, and user mount permission"]
fn fuse_mount_supports_create_truncate_and_offset_writes() -> io::Result<()> {
    if !host_can_run_fuse() {
        return Ok(());
    }

    let root = unique_temp_dir("r9p-fuse-write-export")?;
    let mountpoint = unique_temp_dir("r9p-fuse-write-mount")?;
    let descriptor = root.with_extension("desc");
    seed_tree(&root)?;

    let mut export = ChildGuard::spawn(
        Command::new(r9p_bin())
            .arg("export")
            .arg("--bind")
            .arg("127.0.0.1:0")
            .arg("--writable")
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
            .arg("--control-timeout")
            .arg("1")
            .arg(endpoint)
            .arg(&mountpoint)
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )?;
    wait_for_mounted_file(&mountpoint.join("dir-0/file-0.txt"))?;

    fs::write(mountpoint.join("dir-0/created.txt"), "created\n")?;
    wait_for_host_file_content(&root.join("dir-0/created.txt"), "created\n")?;

    let target = mountpoint.join("dir-0/file-0.txt");
    fs::write(&target, "short\n")?;
    wait_for_host_file_content(&root.join("dir-0/file-0.txt"), "short\n")?;

    let mut offset_write = OpenOptions::new().write(true).open(&target)?;
    offset_write.seek(SeekFrom::Start(3))?;
    offset_write.write_all(b"++")?;
    offset_write.flush()?;
    drop(offset_write);
    wait_for_host_file_content(&root.join("dir-0/file-0.txt"), "sho++\n")?;

    unmount(&mountpoint);
    mount.wait_or_kill()?;
    export.kill();
    export.wait_or_kill()?;
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&mountpoint);
    let _ = fs::remove_file(descriptor);
    Ok(())
}

#[test]
#[ignore = "host-gated: requires /dev/fuse, fusermount, and user mount permission"]
fn fuse_mount_replays_read_only_open_handle_after_export_restart() -> io::Result<()> {
    if !host_can_run_fuse() {
        return Ok(());
    }

    let root = unique_temp_dir("r9p-fuse-replay-export")?;
    let mountpoint = unique_temp_dir("r9p-fuse-replay-mount")?;
    let descriptor = root.with_extension("desc");
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
            .arg("--control-timeout")
            .arg("1")
            .arg(&endpoint)
            .arg(&mountpoint)
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )?;
    let mounted_file = mountpoint.join("dir-0/file-0.txt");
    wait_for_mounted_file(&mounted_file)?;
    let mut open_file = fs::File::open(&mounted_file)?;

    export.kill();
    export.wait_or_kill()?;
    let mut restarted_export = ChildGuard::spawn(
        Command::new(r9p_bin())
            .arg("export")
            .arg("--bind")
            .arg(&endpoint)
            .arg(&root)
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )?;
    wait_for_mounted_file(&mounted_file)?;

    let mut contents = String::new();
    open_file.read_to_string(&mut contents)?;
    if !contents.contains("dir=0 file=0") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "replayed open handle returned unexpected contents",
        ));
    }

    unmount(&mountpoint);
    mount.wait_or_kill()?;
    restarted_export.kill();
    restarted_export.wait_or_kill()?;
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&mountpoint);
    let _ = fs::remove_file(descriptor);
    Ok(())
}

#[test]
#[ignore = "host-gated: requires /dev/fuse, fusermount, and user mount permission"]
fn fuse_mount_lazy_unmount_drains_open_handles() -> io::Result<()> {
    if !host_can_run_fuse() {
        return Ok(());
    }

    let root = unique_temp_dir("r9p-fuse-lazy-export")?;
    let mountpoint = unique_temp_dir("r9p-fuse-lazy-mount")?;
    let descriptor = root.with_extension("desc");
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
            .arg("--control-timeout")
            .arg("1")
            .arg(endpoint)
            .arg(&mountpoint)
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )?;
    let mounted_file = mountpoint.join("dir-0/file-0.txt");
    wait_for_mounted_file(&mounted_file)?;

    let mut open_file = fs::File::open(&mounted_file)?;
    lazy_unmount(&mountpoint)?;
    wait_for_mount_detached(&mountpoint)?;

    let mut contents = String::new();
    open_file.read_to_string(&mut contents)?;
    if !contents.contains("dir=0 file=0") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "open handle returned unexpected contents after lazy unmount",
        ));
    }
    drop(open_file);

    mount.wait_until_exit(Duration::from_secs(5))?;
    export.kill();
    export.wait_or_kill()?;
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&mountpoint);
    let _ = fs::remove_file(descriptor);
    Ok(())
}

#[test]
#[ignore = "host-gated: requires /dev/fuse, fusermount, and user mount permission"]
fn fuse_mount_sigterm_releases_waiting_readers() -> io::Result<()> {
    if !host_can_run_fuse() {
        return Ok(());
    }

    let mountpoint = unique_temp_dir("r9p-fuse-sigterm-mount")?;
    let server = SlowServer::start()?;

    let mut mount = ChildGuard::spawn(
        Command::new(r9p_bin())
            .arg("mount")
            .arg("--request-timeout")
            .arg("30")
            .arg("--read-timeout")
            .arg("30")
            .arg("--control-timeout")
            .arg("1")
            .arg(&server.endpoint)
            .arg(&mountpoint)
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )?;
    wait_for_metadata(&mountpoint.join("slow"))?;

    let mut reader = ChildGuard::spawn(
        Command::new("head")
            .arg("-c")
            .arg("1")
            .arg(mountpoint.join("slow"))
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )?;
    server.wait_until_read_started()?;

    mount.signal(libc::SIGTERM);
    reader.wait_until_exit(Duration::from_secs(5))?;
    mount.wait_until_exit(Duration::from_secs(5))?;
    wait_for_mount_detached(&mountpoint)?;

    server.release();
    let _ = server.join();
    let _ = fs::remove_dir_all(&mountpoint);
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

fn wait_for_host_file_content(path: &Path, expected: &str) -> io::Result<()> {
    let started = Instant::now();
    loop {
        match fs::read_to_string(path) {
            Ok(contents) if contents == expected => return Ok(()),
            Ok(contents) if started.elapsed() > Duration::from_secs(5) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unexpected host file contents {contents:?}"),
                ));
            }
            Ok(_) => {}
            Err(error) if started.elapsed() > Duration::from_secs(5) => return Err(error),
            Err(_) => {}
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_metadata(path: &Path) -> io::Result<()> {
    let started = Instant::now();
    loop {
        match fs::metadata(path) {
            Ok(_) => return Ok(()),
            Err(error) if started.elapsed() > Duration::from_secs(5) => return Err(error),
            Err(_) => {}
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

fn lazy_unmount(path: &Path) -> io::Result<()> {
    for command in [
        ("fusermount3", vec!["-u", "-z"]),
        ("fusermount", vec!["-u", "-z"]),
        ("umount", vec!["-l"]),
    ] {
        let (binary, args) = command;
        if Command::new(binary)
            .args(args)
            .arg(path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
        {
            return Ok(());
        }
    }
    Err(io::Error::other("lazy unmount failed"))
}

fn wait_for_mount_detached(path: &Path) -> io::Result<()> {
    let started = Instant::now();
    while started.elapsed() <= Duration::from_secs(5) {
        if !is_mountpoint(path) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "mountpoint did not detach",
    ))
}

fn is_mountpoint(path: &Path) -> bool {
    Command::new("findmnt")
        .arg("--mountpoint")
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
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

    fn signal(&mut self, signal: libc::c_int) {
        if let Some(child) = &mut self.child {
            unsafe {
                libc::kill(
                    libc::pid_t::try_from(child.id()).unwrap_or(libc::pid_t::MAX),
                    signal,
                );
            }
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

    fn wait_until_exit(&mut self, timeout: Duration) -> io::Result<()> {
        if let Some(mut child) = self.child.take() {
            let started = Instant::now();
            loop {
                if child.try_wait()?.is_some() {
                    let _ = child.wait()?;
                    return Ok(());
                }
                if started.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "child did not exit after lazy unmount",
                    ));
                }
                thread::sleep(Duration::from_millis(20));
            }
        }
        Ok(())
    }
}

struct SlowServer {
    endpoint: String,
    read_calls: Arc<AtomicUsize>,
    release: Arc<AtomicBool>,
    handle: JoinHandle<io::Result<()>>,
}

impl SlowServer {
    fn start() -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let endpoint = listener.local_addr()?.to_string();
        let read_calls = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(AtomicBool::new(false));
        let thread_read_calls = Arc::clone(&read_calls);
        let thread_release = Arc::clone(&release);
        let handle = thread::spawn(move || -> io::Result<()> {
            let (stream, _) = listener.accept()?;
            serve_slow_connection(stream, thread_read_calls, thread_release)
        });
        Ok(Self {
            endpoint,
            read_calls,
            release,
            handle,
        })
    }

    fn wait_until_read_started(&self) -> io::Result<()> {
        let started = Instant::now();
        while started.elapsed() <= Duration::from_secs(5) {
            if self.read_calls.load(Ordering::SeqCst) > 0 {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(20));
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "slow read did not start",
        ))
    }

    fn release(&self) {
        self.release.store(true, Ordering::SeqCst);
    }

    fn join(self) -> io::Result<()> {
        self.handle
            .join()
            .map_err(|_| io::Error::other("slow server thread panicked"))?
    }
}

struct SlowTree {
    root: Qid,
    slow: Qid,
    read_calls: Arc<AtomicUsize>,
    release: Arc<AtomicBool>,
}

impl SlowTree {
    fn new(read_calls: Arc<AtomicUsize>, release: Arc<AtomicBool>) -> Self {
        Self {
            root: Qid::dir(1),
            slow: Qid::file(2),
            read_calls,
            release,
        }
    }
}

impl FileTree for SlowTree {
    fn attach(&mut self, _fid: Fid, _uname: &[u8], _aname: &[u8]) -> R9pResult<Qid> {
        Ok(self.root)
    }

    fn walk(
        &mut self,
        _fid: Fid,
        _newfid: Fid,
        start: Qid,
        names: &[Vec<u8>],
    ) -> R9pResult<Vec<Qid>> {
        match names {
            [name] if start == self.root && name == b"slow" => Ok(vec![self.slow]),
            _ => Ok(Vec::new()),
        }
    }

    fn open(&mut self, _fid: Fid, qid: Qid, _mode: u8) -> R9pResult<OpenFile> {
        Ok(OpenFile { qid, iounit: 0 })
    }

    fn read(&mut self, _fid: Fid, qid: Qid, offset: u64, _count: u32) -> R9pResult<ReadData> {
        if qid != self.slow {
            return Ok(ReadData::Directory(Vec::new()));
        }
        self.read_calls.fetch_add(1, Ordering::SeqCst);
        while !self.release.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(20));
        }
        if offset > 0 {
            Ok(ReadData::Bytes(Vec::new()))
        } else {
            Ok(ReadData::Bytes(b"x".to_vec()))
        }
    }

    fn stat(&mut self, qid: Qid) -> R9pResult<Stat> {
        if qid == self.slow {
            let mut stat = Stat::new("slow", qid, 0o444);
            stat.length = 1;
            Ok(stat)
        } else {
            Ok(Stat::new(".", qid, DMDIR | 0o555))
        }
    }
}

fn serve_slow_connection(
    mut stream: TcpStream,
    read_calls: Arc<AtomicUsize>,
    release: Arc<AtomicBool>,
) -> io::Result<()> {
    let mut server = Server::new(SlowTree::new(read_calls, release));
    while let Some(message) = read_tmessage(&mut stream)? {
        let reply = server.handle(message);
        let frame = codec::encode_rmessage_checked(&reply, server.session().msize())
            .map_err(|error| io::Error::other(format!("encode reply: {error}")))?;
        if let Err(error) = stream.write_all(&frame) {
            if matches!(
                error.kind(),
                io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
            ) {
                return Ok(());
            }
            return Err(error);
        }
    }
    Ok(())
}

fn read_tmessage(stream: &mut TcpStream) -> io::Result<Option<TMessage>> {
    let mut prefix = [0_u8; 4];
    match stream.read_exact(&mut prefix) {
        Ok(()) => {}
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    }
    let size = u32::from_le_bytes(prefix);
    if size < codec::FRAME_HEADER_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("short frame: {size}"),
        ));
    }
    let rest_len = usize::try_from(size - 4)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "frame too large"))?;
    let mut frame = Vec::with_capacity(rest_len + 4);
    frame.extend(prefix);
    frame.resize(rest_len + 4, 0);
    stream.read_exact(&mut frame[4..])?;
    codec::decode_tmessage(&frame)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.kill();
        let _ = self.wait_or_kill();
    }
}
