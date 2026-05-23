//! `/dev/fuse` acquisition via `fusermount` / `fusermount3`.
//!
//! The kernel hands us a single file descriptor representing the FUSE channel;
//! we obtain it by exec'ing the helper binary and reading the fd off the
//! shared `SCM_RIGHTS` socket.
//!
//! We also install a SIGINT/SIGTERM/SIGHUP watcher that runs `fusermount -u`
//! on the way out. Without it, an interrupted process leaves the mount in
//! `Transport endpoint is not connected` limbo until the user manually runs
//! the unmount.

use crate::error::{Error, Result};
use std::{
    ffi::CString,
    mem::zeroed,
    os::fd::RawFd,
    path::{Path, PathBuf},
    process::Command,
    ptr, thread,
};

pub(super) fn mount_fuse(mountpoint: &Path) -> Result<RawFd> {
    let mountpoint_str = mountpoint
        .to_str()
        .ok_or_else(|| Error::new(libc::EINVAL, "mountpoint is not valid UTF-8"))?;
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
            child_exec_fusermount(sockets[0], mountpoint_str);
        }
    }
    unsafe {
        libc::close(sockets[0]);
    }
    let fd = recv_fd(sockets[1]);
    unsafe {
        libc::close(sockets[1]);
    }
    let mut status = 0_i32;
    unsafe {
        libc::waitpid(pid, &mut status, 0);
    }
    let fd = fd?;
    install_unmount_on_signal(mountpoint.to_path_buf());
    Ok(fd)
}

unsafe fn child_exec_fusermount(comm_fd: RawFd, mountpoint: &str) -> ! {
    let env_name = CString::new("_FUSE_COMMFD").expect("static env name contains no NUL");
    let env_value = CString::new(comm_fd.to_string()).expect("fd string contains no NUL");
    let mountpoint = CString::new(mountpoint).expect("mountpoint contains no NUL");
    libc::setenv(env_name.as_ptr(), env_value.as_ptr(), 1);

    let dashdash = CString::new("--").expect("static arg contains no NUL");
    for binary in fusermount_candidates() {
        let fusermount = CString::new(binary).expect("static command contains no NUL");
        libc::execlp(
            fusermount.as_ptr(),
            fusermount.as_ptr(),
            dashdash.as_ptr(),
            mountpoint.as_ptr(),
            ptr::null::<libc::c_char>(),
        );
    }
    libc::_exit(1);
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

fn install_unmount_on_signal(mountpoint: PathBuf) {
    // Block SIGINT/SIGTERM/SIGHUP on every thread except the dedicated
    // watcher. Setting the mask on the calling (main) thread before any
    // worker threads spawn means the watcher's sibling threads inherit the
    // block, which is what `sigwait` requires.
    unsafe {
        let mut set: libc::sigset_t = zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGHUP);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, ptr::null_mut());
    }
    thread::spawn(move || {
        let signo = wait_for_termination_signal();
        run_fusermount_unmount(&mountpoint);
        // Re-raise so the process exit code reflects the originating signal.
        unsafe {
            let mut set: libc::sigset_t = zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, signo);
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, ptr::null_mut());
            libc::raise(signo);
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

fn run_fusermount_unmount(mountpoint: &Path) {
    for binary in fusermount_candidates() {
        if Command::new(binary)
            .arg("-u")
            .arg(mountpoint)
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
        {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fusermount_candidates;

    #[test]
    fn prefers_fusermount3_before_fuse2_helper() {
        assert_eq!(["fusermount3", "fusermount"], fusermount_candidates());
    }
}
