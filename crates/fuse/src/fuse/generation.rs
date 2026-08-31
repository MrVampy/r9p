use super::{mount::MountCleanup, status::MountStatus};
use crate::{error::Result, Error};
use std::{
    env,
    ffi::OsStr,
    mem::zeroed,
    os::{
        linux::net::SocketAddrExt,
        unix::{
            ffi::OsStrExt,
            net::{SocketAddr, UnixDatagram},
        },
    },
    ptr,
    sync::{Arc, Condvar, Mutex},
    thread,
};

pub(super) struct MountGeneration {
    replacement: Arc<(Mutex<ReplacementState>, Condvar)>,
}

#[derive(Default)]
struct ReplacementState {
    pending: bool,
    successor_ready: bool,
}

impl MountGeneration {
    pub(super) fn start(cleanup: MountCleanup, status: MountStatus) -> Result<Self> {
        let replacement = Arc::new((Mutex::new(ReplacementState::default()), Condvar::new()));
        let signal_replacement = Arc::clone(&replacement);
        thread::Builder::new()
            .name("r9p-mount-signals".to_string())
            .spawn(move || signal_loop(cleanup, status, signal_replacement))
            .map_err(|error| Error::io("spawn mount signal watcher", error))?;
        Ok(Self { replacement })
    }

    pub(super) fn wait_for_successor_if_pending(&self) {
        let (state, ready) = &*self.replacement;
        let Ok(mut state) = state.lock() else {
            return;
        };
        while state.pending && !state.successor_ready {
            let Ok(next) = ready.wait(state) else {
                return;
            };
            state = next;
        }
    }
}

pub(super) fn block_mount_signals() {
    set_mount_signal_mask(libc::SIG_BLOCK);
}

pub(super) fn unblock_mount_signals() {
    set_mount_signal_mask(libc::SIG_UNBLOCK);
}

pub(super) fn notify_ready(adopt_main_process: bool) -> Result<()> {
    let Some(address) = env::var_os("NOTIFY_SOCKET") else {
        return Ok(());
    };
    let address = address.as_encoded_bytes();
    let socket_address = if address.first() == Some(&b'@') {
        SocketAddr::from_abstract_name(&address[1..])
            .map_err(|error| Error::io("parse abstract systemd notify socket", error))?
    } else {
        SocketAddr::from_pathname(std::path::Path::new(OsStr::from_bytes(address)))
            .map_err(|error| Error::io("parse systemd notify socket", error))?
    };
    let message = if adopt_main_process {
        format!("MAINPID={}\nREADY=1", std::process::id())
    } else {
        "READY=1".to_string()
    };
    UnixDatagram::unbound()
        .and_then(|socket| socket.send_to_addr(message.as_bytes(), &socket_address))
        .map(|_| ())
        .map_err(|error| Error::io("notify systemd mount readiness", error))
}

fn signal_loop(
    cleanup: MountCleanup,
    status: MountStatus,
    replacement: Arc<(Mutex<ReplacementState>, Condvar)>,
) {
    loop {
        match wait_for_mount_signal() {
            libc::SIGHUP => {
                if begin_replacement(&replacement) {
                    status.retire();
                    cleanup.detach_for_replacement();
                }
            }
            libc::SIGUSR1 => complete_replacement(&replacement),
            signo => {
                cleanup.cleanup();
                unsafe {
                    libc::_exit(128 + signo);
                }
            }
        }
    }
}

fn begin_replacement(replacement: &Arc<(Mutex<ReplacementState>, Condvar)>) -> bool {
    let (state, _) = &**replacement;
    let Ok(mut state) = state.lock() else {
        return false;
    };
    if state.pending {
        return false;
    }
    state.pending = true;
    true
}

fn complete_replacement(replacement: &Arc<(Mutex<ReplacementState>, Condvar)>) {
    let (state, ready) = &**replacement;
    let Ok(mut state) = state.lock() else {
        return;
    };
    if state.pending {
        state.successor_ready = true;
        ready.notify_all();
    }
}

fn set_mount_signal_mask(how: libc::c_int) {
    unsafe {
        let mut set: libc::sigset_t = zeroed();
        libc::sigemptyset(&mut set);
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGUSR1] {
            libc::sigaddset(&mut set, signal);
        }
        libc::pthread_sigmask(how, &set, ptr::null_mut());
    }
}

fn wait_for_mount_signal() -> libc::c_int {
    unsafe {
        let mut set: libc::sigset_t = zeroed();
        libc::sigemptyset(&mut set);
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGUSR1] {
            libc::sigaddset(&mut set, signal);
        }
        let mut signo = 0;
        if libc::sigwait(&set, &mut signo) != 0 {
            libc::SIGTERM
        } else {
            signo
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{begin_replacement, complete_replacement, ReplacementState};
    use std::sync::{Arc, Condvar, Mutex};

    #[test]
    fn replacement_fence_is_one_way_and_requires_pending_generation() {
        let replacement = Arc::new((Mutex::new(ReplacementState::default()), Condvar::new()));
        complete_replacement(&replacement);
        {
            let state = replacement.0.lock().expect("replacement state");
            assert!(!state.successor_ready);
        }
        assert!(begin_replacement(&replacement));
        assert!(!begin_replacement(&replacement));
        complete_replacement(&replacement);
        let state = replacement.0.lock().expect("replacement state");
        assert!(state.pending);
        assert!(state.successor_ready);
    }
}
