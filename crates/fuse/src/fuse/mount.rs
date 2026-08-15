//! `/dev/fuse` acquisition via `fusermount` / `fusermount3`.
//!
//! The kernel hands us a single file descriptor representing the FUSE channel;
//! we obtain it by exec'ing the helper binary and reading the fd off the
//! shared `SCM_RIGHTS` socket. When `auto_unmount` is active, that socket is
//! also the helper's liveness guard, so the mount owns both the socket and the
//! helper process until teardown.
//!
//! We also install a SIGINT/SIGTERM/SIGHUP watcher that runs `fusermount -u`
//! on the way out. Without it, an interrupted process leaves the mount in
//! `Transport endpoint is not connected` limbo until the user manually runs
//! the unmount.

use crate::error::{Error, Result};
use std::{
    ffi::CString,
    fs::{self, File},
    io,
    mem::zeroed,
    os::fd::{FromRawFd, OwnedFd, RawFd},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    ptr,
    sync::{
        atomic::{AtomicI32, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

const UNMOUNT_TIMEOUT: Duration = Duration::from_secs(2);

pub(super) struct FuseMount {
    file: Option<File>,
    cleanup: MountCleanup,
}

pub(super) fn block_termination_signals() {
    set_termination_signal_mask(libc::SIG_BLOCK);
}

fn unblock_termination_signals() {
    set_termination_signal_mask(libc::SIG_UNBLOCK);
}

fn set_termination_signal_mask(how: libc::c_int) {
    unsafe {
        let mut set: libc::sigset_t = zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGHUP);
        libc::pthread_sigmask(how, &set, ptr::null_mut());
    }
}

impl FuseMount {
    pub(super) fn file_mut(&mut self) -> &mut File {
        self.file.as_mut().expect("FUSE mount file missing")
    }

    pub(super) fn cleanup_handle(&self) -> MountCleanup {
        self.cleanup.clone()
    }
}

impl Drop for FuseMount {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            if self.cleanup.fuse_fd_is_open() {
                drop(file);
                self.cleanup.mark_fuse_fd_closed();
            } else {
                std::mem::forget(file);
            }
        }
        self.cleanup.detach_and_abort();
    }
}

#[derive(Clone)]
pub(super) struct MountCleanup {
    mountpoint: PathBuf,
    connection_id: Option<u64>,
    fuse_fd: Arc<AtomicI32>,
    auto_unmount_guardian: Arc<Mutex<Option<AutoUnmountGuardian>>>,
}

impl MountCleanup {
    pub(super) fn cleanup(&self) {
        self.close_fuse_fd();
        self.detach_and_abort();
    }

    fn detach_and_abort(&self) {
        abort_fuse_connection(self.connection_id);
        self.stop_auto_unmount_guardian();
        lazy_unmount(&self.mountpoint);
    }

    fn stop_auto_unmount_guardian(&self) {
        let guardian = self
            .auto_unmount_guardian
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(guardian) = guardian {
            let _ = guardian.stop();
        }
    }

    fn close_fuse_fd(&self) {
        let fd = self.fuse_fd.swap(-1, Ordering::SeqCst);
        if fd >= 0 {
            unsafe {
                libc::close(fd);
            }
        }
    }

    fn mark_fuse_fd_closed(&self) {
        self.fuse_fd.store(-1, Ordering::SeqCst);
    }

    fn fuse_fd_is_open(&self) -> bool {
        self.fuse_fd.load(Ordering::SeqCst) >= 0
    }
}

pub(super) fn mount_fuse(
    mountpoint: &Path,
    namespace_source: &str,
    allow_other: bool,
    unmount_on_signal: bool,
) -> Result<FuseMount> {
    let absolute_mountpoint = absolute_mountpoint(mountpoint)?;
    clear_stale_fuse_mount(&absolute_mountpoint);
    let mountpoint_str = absolute_mountpoint
        .to_str()
        .ok_or_else(|| Error::new(libc::EINVAL, "mountpoint is not valid UTF-8"))?
        .to_string();
    let filesystem_source = filesystem_source(namespace_source);
    let descriptors =
        match mount_fuse_attempt(&mountpoint_str, &filesystem_source, true, allow_other) {
            Ok(descriptors) => descriptors,
            Err(_) => mount_fuse_attempt(&mountpoint_str, &filesystem_source, false, allow_other)?,
        };
    let fd = descriptors.fuse_fd;
    let connection_id = connection_id_for_mountpoint(&mountpoint_str);
    let fuse_fd = Arc::new(AtomicI32::new(fd));
    let cleanup = MountCleanup {
        mountpoint: absolute_mountpoint,
        connection_id,
        fuse_fd: Arc::clone(&fuse_fd),
        auto_unmount_guardian: Arc::new(Mutex::new(descriptors.auto_unmount_guardian)),
    };
    if unmount_on_signal {
        install_unmount_on_signal(cleanup.clone());
    }
    Ok(FuseMount {
        file: Some(unsafe { File::from_raw_fd(fd) }),
        cleanup,
    })
}

fn mount_fuse_attempt(
    mountpoint_str: &str,
    filesystem_source: &str,
    auto_unmount: bool,
    allow_other: bool,
) -> Result<MountDescriptors> {
    let mut sockets = [0_i32; 2];
    let rc = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sockets.as_mut_ptr()) };
    if rc < 0 {
        return Err(Error::io("socketpair", std::io::Error::last_os_error()));
    }
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        unsafe {
            libc::close(sockets[0]);
            libc::close(sockets[1]);
        }
        return Err(Error::io("fork", std::io::Error::last_os_error()));
    }
    if pid == 0 {
        unsafe {
            libc::close(sockets[1]);
            child_exec_fusermount(
                sockets[0],
                mountpoint_str,
                filesystem_source,
                auto_unmount,
                allow_other,
            );
        }
    }
    unsafe {
        libc::close(sockets[0]);
    }
    let fuse_fd = recv_fd(sockets[1]);
    match (auto_unmount, fuse_fd) {
        (true, Ok(fuse_fd)) => {
            if let Err(error) = set_close_on_exec(sockets[1]) {
                unsafe {
                    libc::close(fuse_fd);
                    libc::close(sockets[1]);
                }
                let _ = wait_for_child_exit(pid);
                return Err(Error::io("protect fusermount auto-unmount guard", error));
            }
            Ok(MountDescriptors {
                fuse_fd,
                auto_unmount_guardian: Some(AutoUnmountGuardian {
                    guard: Some(unsafe { OwnedFd::from_raw_fd(sockets[1]) }),
                    pid,
                }),
            })
        }
        (_, fuse_fd) => {
            unsafe {
                libc::close(sockets[1]);
            }
            let _ = wait_for_child_exit(pid);
            fuse_fd.map(|fuse_fd| MountDescriptors {
                fuse_fd,
                auto_unmount_guardian: None,
            })
        }
    }
}

struct MountDescriptors {
    fuse_fd: RawFd,
    auto_unmount_guardian: Option<AutoUnmountGuardian>,
}

struct AutoUnmountGuardian {
    guard: Option<OwnedFd>,
    pid: libc::pid_t,
}

impl AutoUnmountGuardian {
    fn stop(mut self) -> io::Result<i32> {
        drop(self.guard.take());
        wait_for_child_exit(self.pid)
    }
}

fn wait_for_child_exit(pid: libc::pid_t) -> io::Result<i32> {
    loop {
        let mut status = 0_i32;
        let result = unsafe { libc::waitpid(pid, &mut status, 0) };
        if result == pid {
            return Ok(status);
        }
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        return Err(io::Error::other("waitpid returned an unexpected child"));
    }
}

fn set_close_on_exec(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn clear_stale_fuse_mount(mountpoint: &Path) {
    for _ in 0..4 {
        if !mountpoint_is_stale_fuse(mountpoint) {
            return;
        }
        lazy_unmount(mountpoint);
    }
}

fn mountpoint_is_stale_fuse(mountpoint: &Path) -> bool {
    if !mountpoint_listed_as_fuse(mountpoint) {
        return false;
    }
    match std::fs::metadata(mountpoint) {
        Ok(_) => false,
        Err(error) => matches!(
            error.raw_os_error(),
            Some(libc::ENOTCONN) | Some(libc::ENOENT) | Some(libc::EIO)
        ),
    }
}

fn mountpoint_listed_as_fuse(mountpoint: &Path) -> bool {
    let Some(target) = mountpoint.to_str() else {
        return false;
    };
    let Ok(mounts) = std::fs::read_to_string("/proc/self/mounts") else {
        return false;
    };
    mounts.lines().any(|line| {
        let mut fields = line.split_whitespace();
        let _device = fields.next();
        let (Some(path), Some(fstype)) = (fields.next(), fields.next()) else {
            return false;
        };
        fstype.starts_with("fuse") && decode_mounts_path(path) == target.as_bytes()
    })
}

fn absolute_mountpoint(mountpoint: &Path) -> Result<PathBuf> {
    if mountpoint.is_absolute() {
        return Ok(mountpoint.to_path_buf());
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(mountpoint))
        .map_err(|error| Error::io("resolve mountpoint path", error))
}

unsafe fn child_exec_fusermount(
    comm_fd: RawFd,
    mountpoint: &str,
    filesystem_source: &str,
    auto_unmount: bool,
    allow_other: bool,
) -> ! {
    unblock_termination_signals();
    let env_name = CString::new("_FUSE_COMMFD").expect("static env name contains no NUL");
    let env_value = CString::new(comm_fd.to_string()).expect("fd string contains no NUL");
    let mountpoint = CString::new(mountpoint).expect("mountpoint contains no NUL");
    libc::setenv(env_name.as_ptr(), env_value.as_ptr(), 1);

    let opt_flag = CString::new("-o").expect("static arg contains no NUL");
    let opt_value = CString::new(mount_options(filesystem_source, auto_unmount, allow_other))
        .expect("validated FUSE options contain no NUL");
    let dashdash = CString::new("--").expect("static arg contains no NUL");
    for binary in fusermount_candidates() {
        let fusermount = CString::new(binary).expect("static command contains no NUL");
        libc::execlp(
            fusermount.as_ptr(),
            fusermount.as_ptr(),
            opt_flag.as_ptr(),
            opt_value.as_ptr(),
            dashdash.as_ptr(),
            mountpoint.as_ptr(),
            ptr::null::<libc::c_char>(),
        );
    }
    libc::_exit(1);
}

fn mount_options(filesystem_source: &str, auto_unmount: bool, allow_other: bool) -> String {
    let mut options = vec![
        "subtype=r9p".to_string(),
        format!("fsname={filesystem_source}"),
    ];
    if auto_unmount {
        options.push("auto_unmount".to_string());
    }
    if allow_other {
        options.push("allow_other".to_string());
    }
    options.join(",")
}

fn filesystem_source(namespace_source: &str) -> String {
    let mut encoded = String::from("r9p:");
    for byte in namespace_source.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

const fn fusermount_candidates() -> [&'static str; 2] {
    ["fusermount3", "fusermount"]
}

pub(super) fn recv_fd(socket: RawFd) -> Result<RawFd> {
    let mut byte = [0_u8; 1];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: byte.len(),
    };
    let mut control = [0_u8; 128];
    let mut msg: libc::msghdr = unsafe { zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = control.len();
    let n = unsafe { libc::recvmsg(socket, &mut msg, 0) };
    if n < 0 {
        return Err(Error::io(
            "recvmsg fusermount fd",
            std::io::Error::last_os_error(),
        ));
    }
    let cmsg = unsafe { libc::CMSG_FIRSTHDR(&msg) };
    if cmsg.is_null() {
        return Err(Error::new(libc::EIO, "fusermount did not pass a FUSE fd"));
    }
    unsafe {
        if (*cmsg).cmsg_level != libc::SOL_SOCKET || (*cmsg).cmsg_type != libc::SCM_RIGHTS {
            return Err(Error::new(
                libc::EIO,
                "fusermount passed unexpected control message",
            ));
        }
        let data = libc::CMSG_DATA(cmsg).cast::<RawFd>();
        Ok(ptr::read_unaligned(data))
    }
}

fn install_unmount_on_signal(cleanup: MountCleanup) {
    // Block SIGINT/SIGTERM/SIGHUP on every thread except the dedicated
    // watcher. Setting the mask on the calling (main) thread before any
    // worker threads spawn means the watcher's sibling threads inherit the
    // block, which is what `sigwait` requires.
    block_termination_signals();
    thread::spawn(move || {
        let signo = wait_for_termination_signal();
        cleanup.cleanup();
        unsafe {
            libc::_exit(128 + signo);
        }
    });
}

fn wait_for_termination_signal() -> libc::c_int {
    unsafe {
        let mut set: libc::sigset_t = zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGHUP);
        let mut signo: libc::c_int = 0;
        if libc::sigwait(&set, &mut signo) != 0 {
            return libc::SIGTERM;
        }
        signo
    }
}

pub(super) fn lazy_unmount(mountpoint: &Path) {
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
    wait_with_timeout(&mut child, UNMOUNT_TIMEOUT).unwrap_or(false)
}

fn wait_with_timeout(child: &mut Child, timeout: Duration) -> io::Result<bool> {
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

fn abort_fuse_connection(connection_id: Option<u64>) {
    let Some(connection_id) = connection_id else {
        return;
    };
    let path = PathBuf::from(format!("/sys/fs/fuse/connections/{connection_id}/abort"));
    let _ = fs::write(path, b"1\n");
}

fn connection_id_for_mountpoint(mountpoint: &str) -> Option<u64> {
    let mountinfo = fs::read_to_string("/proc/self/mountinfo").ok()?;
    parse_connection_id_from_mountinfo(&mountinfo, mountpoint)
}

fn parse_connection_id_from_mountinfo(mountinfo: &str, mountpoint: &str) -> Option<u64> {
    for line in mountinfo.lines() {
        let fields = line
            .split(" - ")
            .next()?
            .split_whitespace()
            .collect::<Vec<_>>();
        if fields.len() < 5 {
            continue;
        }
        if decode_mounts_path(fields[4]) != mountpoint.as_bytes() {
            continue;
        }
        let (_major, minor) = fields[2].split_once(':')?;
        return minor.parse().ok();
    }
    None
}

fn decode_mounts_path(path: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(path.len());
    let mut bytes = path.bytes().peekable();
    while let Some(byte) = bytes.next() {
        if byte != b'\\' {
            out.push(byte);
            continue;
        }
        let mut digits = [0_u8; 3];
        let mut count = 0;
        while count < 3 {
            match bytes.peek() {
                Some(&digit @ b'0'..=b'7') => {
                    digits[count] = digit;
                    bytes.next();
                    count += 1;
                }
                _ => break,
            }
        }
        let value = digits[..count]
            .iter()
            .fold(0_u32, |acc, digit| acc * 8 + u32::from(digit - b'0'));
        if count == 3 && value <= 0xFF {
            out.push(value as u8);
        } else {
            out.push(b'\\');
            out.extend_from_slice(&digits[..count]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        decode_mounts_path, filesystem_source, fusermount_candidates, mount_options,
        parse_connection_id_from_mountinfo, AutoUnmountGuardian,
    };
    use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};
    use std::os::unix::net::UnixStream;

    #[test]
    fn prefers_fusermount3_before_fuse2_helper() {
        assert_eq!(["fusermount3", "fusermount"], fusermount_candidates());
    }

    #[test]
    fn renders_namespace_provenance_and_explicit_fuse_access_options() {
        let source = filesystem_source("/sources/newsgroups/browse");
        assert_eq!(source, "r9p:%2Fsources%2Fnewsgroups%2Fbrowse");
        assert_eq!(
            "subtype=r9p,fsname=r9p:%2Fsources%2Fnewsgroups%2Fbrowse",
            mount_options(&source, false, false)
        );
        assert_eq!(
            "subtype=r9p,fsname=r9p:%2Fsources%2Fnewsgroups%2Fbrowse,auto_unmount,allow_other",
            mount_options(&source, true, true)
        );
    }

    #[test]
    fn filesystem_source_escapes_fuse_option_delimiters_and_utf8() {
        assert_eq!(
            filesystem_source("/sources/a,b/cover æ"),
            "r9p:%2Fsources%2Fa%2Cb%2Fcover%20%C3%A6"
        );
    }

    #[test]
    fn owned_auto_unmount_guard_signals_and_reaps_helper() {
        let (guard, peer) = UnixStream::pair().expect("auto-unmount guard socket pair");
        let guard_fd = guard.into_raw_fd();
        let peer_fd = peer.into_raw_fd();
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork auto-unmount test helper");
        if pid == 0 {
            unsafe {
                libc::close(guard_fd);
                let mut byte = 0_u8;
                let read = libc::read(peer_fd, (&mut byte as *mut u8).cast(), 1);
                libc::close(peer_fd);
                libc::_exit(if read == 0 { 0 } else { 1 });
            }
        }

        unsafe {
            libc::close(peer_fd);
        }
        let guardian = AutoUnmountGuardian {
            guard: Some(unsafe { OwnedFd::from_raw_fd(guard_fd) }),
            pid,
        };
        let status = guardian.stop().expect("stop and reap auto-unmount helper");
        assert!(libc::WIFEXITED(status));
        assert_eq!(0, libc::WEXITSTATUS(status));
    }

    #[test]
    fn parses_fuse_connection_id_from_mountinfo() {
        let mountinfo = concat!(
            "42 28 0:37 / /sys/fs/fuse/connections rw - fusectl fusectl rw\n",
            "68 30 0:57 / /home/mrvamp/example/.vault/live rw - fuse /dev/fuse rw,user_id=1000\n",
        );
        assert_eq!(
            Some(57),
            parse_connection_id_from_mountinfo(mountinfo, "/home/mrvamp/example/.vault/live")
        );
    }

    #[test]
    fn decodes_mountinfo_octal_escapes() {
        assert_eq!(
            b"/tmp/r9p mount/live".to_vec(),
            decode_mounts_path("/tmp/r9p\\040mount/live")
        );
    }

    #[test]
    fn decodes_multibyte_utf8_mount_paths_bytewise() {
        assert_eq!(
            "/tmp/r9pø/live".as_bytes().to_vec(),
            decode_mounts_path("/tmp/r9p\\303\\270/live")
        );
        let mountinfo =
            "68 30 0:57 / /home/mrvamp/V\\303\\270lt/.vault/live rw - fuse /dev/fuse rw\n";
        assert_eq!(
            Some(57),
            parse_connection_id_from_mountinfo(mountinfo, "/home/mrvamp/Vølt/.vault/live")
        );
    }

    #[test]
    fn preserves_malformed_octal_escapes_without_panicking() {
        assert_eq!(b"/tmp/\\777x".to_vec(), decode_mounts_path("/tmp/\\777x"));
        assert_eq!(b"/tmp/\\47x".to_vec(), decode_mounts_path("/tmp/\\47x"));
        assert_eq!(b"/tmp/end\\".to_vec(), decode_mounts_path("/tmp/end\\"));
    }

    #[test]
    fn relative_mountpoint_resolution_is_lexical() {
        let cwd = std::env::current_dir().expect("current dir");
        assert_eq!(
            cwd.join(".vault/live"),
            super::absolute_mountpoint(std::path::Path::new(".vault/live")).expect("absolute path")
        );
    }
}
