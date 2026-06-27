use std::{fmt, io};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub struct Error {
    pub errno: i32,
    message: String,
}

impl Error {
    pub fn new(errno: i32, message: impl Into<String>) -> Self {
        Self {
            errno,
            message: message.into(),
        }
    }

    pub fn io(context: impl AsRef<str>, error: io::Error) -> Self {
        let errno = error.raw_os_error().unwrap_or(libc::EIO);
        Self::new(errno, format!("{}: {error}", context.as_ref()))
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.message, self.errno)
    }
}

impl std::error::Error for Error {}

pub fn p9_error(ename: &[u8]) -> Error {
    let message = String::from_utf8_lossy(ename).to_string();
    Error::new(errno_for_9p_error(&message), message)
}

pub fn errno_for_9p_error(message: &str) -> i32 {
    let lower = message.to_ascii_lowercase();
    for (pattern, errno) in PLAN9_ERRNO_PATTERNS {
        if lower.contains(pattern) {
            return *errno;
        }
    }
    libc::EREMOTEIO
}

// 9P2000 Rerror carries text, not numeric errno. The bridge still has to reply
// to Linux FUSE with errno, so keep this table broad and POSIX-shaped. The
// order matters: specific phrases like "not a directory" must beat generic
// "directory", and "does not exist" must beat "exists".
const PLAN9_ERRNO_PATTERNS: &[(&str, i32)] = &[
    ("unknown fid", libc::ESTALE),
    ("stale fid", libc::ESTALE),
    ("does not exist", libc::ENOENT),
    ("no such device", libc::ENODEV),
    ("not found", libc::ENOENT),
    ("not_found", libc::ENOENT),
    ("does_not_exist", libc::ENOENT),
    ("not exist", libc::ENOENT),
    ("no such file", libc::ENOENT),
    ("no such entry", libc::ENOENT),
    ("no such", libc::ENOENT),
    ("bad walk", libc::ENOENT),
    ("walk_partial", libc::ENOENT),
    ("walk failed", libc::ENOENT),
    ("walk", libc::ENOENT),
    ("range", libc::ENOENT),
    ("operation not permitted", libc::EPERM),
    ("not permitted", libc::EPERM),
    ("forbidden", libc::EACCES),
    ("unauthorized", libc::EACCES),
    ("permission_denied", libc::EACCES),
    ("permission", libc::EACCES),
    ("access", libc::EACCES),
    ("denied", libc::EACCES),
    ("not writable", libc::EACCES),
    ("not_writable", libc::EACCES),
    ("write not allowed", libc::EACCES),
    ("not readable", libc::EACCES),
    ("not_readable", libc::EACCES),
    ("already exists", libc::EEXIST),
    ("file exists", libc::EEXIST),
    (" exists", libc::EEXIST),
    ("duplicate", libc::EEXIST),
    ("not a directory", libc::ENOTDIR),
    ("not directory", libc::ENOTDIR),
    ("not dir", libc::ENOTDIR),
    ("is a directory", libc::EISDIR),
    ("is directory", libc::EISDIR),
    ("directory", libc::ENOTDIR),
    ("not empty", libc::ENOTEMPTY),
    ("preflight", libc::EINVAL),
    ("rejected", libc::EINVAL),
    ("decode", libc::EINVAL),
    ("decode_failed", libc::EINVAL),
    ("invalid", libc::EINVAL),
    ("illegal", libc::EINVAL),
    ("argument", libc::EINVAL),
    ("malformed", libc::EINVAL),
    ("parse", libc::EINVAL),
    ("parse_failed", libc::EINVAL),
    ("bad", libc::EINVAL),
    // Never pass remote "not implemented" through as FUSE ENOSYS. Linux
    // caches ENOSYS per opcode for the mount lifetime; a backend rejection for
    // one read/write/lookup must not brick that FUSE opcode until remount.
    ("not implemented", libc::ENOTSUP),
    ("not_implemented", libc::ENOTSUP),
    ("unimplemented", libc::ENOTSUP),
    ("unsupported", libc::ENOTSUP),
    ("not_supported", libc::ENOTSUP),
    ("not supported", libc::ENOTSUP),
    ("op unsupported", libc::ENOTSUP),
    ("read-only", libc::EROFS),
    ("read only", libc::EROFS),
    ("timed out", libc::ETIMEDOUT),
    ("timed_out", libc::ETIMEDOUT),
    ("client_command_timeout", libc::ETIMEDOUT),
    ("timeout", libc::ETIMEDOUT),
    ("interrupt", libc::EINTR),
    ("bad message", libc::EBADMSG),
    ("bad file", libc::EBADF),
    ("input/output", libc::EIO),
    ("i/o", libc::EIO),
    ("protocol", libc::EPROTO),
    ("proto", libc::EPROTO),
    ("no connection", libc::ENOTCONN),
    ("connection lost", libc::ECONNABORTED),
    ("connection reset", libc::ECONNRESET),
    ("pipe", libc::EPIPE),
    ("temporar", libc::EAGAIN),
    ("unavailable", libc::EAGAIN),
    ("generation_conflict", libc::EAGAIN),
    ("out of memory", libc::ENOMEM),
    ("memory", libc::ENOMEM),
    ("name too long", libc::ENAMETOOLONG),
    ("too long", libc::E2BIG),
    ("too large", libc::EFBIG),
    ("overflow", libc::EOVERFLOW),
    ("in use", libc::EBUSY),
    ("busy", libc::EBUSY),
];

#[cfg(test)]
mod tests {
    use super::errno_for_9p_error;

    #[test]
    fn unknown_fid_is_stale_not_remote_io() {
        assert_eq!(errno_for_9p_error("unknown fid"), libc::ESTALE);
    }

    #[test]
    fn preflight_failure_is_invalid_argument() {
        assert_eq!(
            errno_for_9p_error("namespace_preflight_failed:missing_required_field"),
            libc::EINVAL
        );
    }

    #[test]
    fn preserves_missing_path_mapping_before_generic_bad() {
        assert_eq!(errno_for_9p_error("bad walk: no such entry"), libc::ENOENT);
    }

    #[test]
    fn maps_common_plan9port_and_network_errors() {
        assert_eq!(errno_for_9p_error("operation not permitted"), libc::EPERM);
        assert_eq!(errno_for_9p_error("not implemented"), libc::ENOTSUP);
        assert_eq!(errno_for_9p_error("read only file system"), libc::EROFS);
        assert_eq!(errno_for_9p_error("timed out"), libc::ETIMEDOUT);
        assert_eq!(errno_for_9p_error("connection lost"), libc::ECONNABORTED);
    }

    #[test]
    fn maps_common_admission_diagnostics() {
        assert_eq!(errno_for_9p_error("preflight failed"), libc::EINVAL);
        assert_eq!(errno_for_9p_error("invalid request shape"), libc::EINVAL);
        assert_eq!(errno_for_9p_error("not writable"), libc::EACCES);
    }

    #[test]
    fn maps_machine_style_errors() {
        assert_eq!(
            errno_for_9p_error("client_command_timeout:read"),
            libc::ETIMEDOUT
        );
        assert_eq!(
            errno_for_9p_error("operation_not_implemented"),
            libc::ENOTSUP
        );
        assert_eq!(
            errno_for_9p_error("namespace_generation_conflict"),
            libc::EAGAIN
        );
        assert_eq!(errno_for_9p_error("not_writable"), libc::EACCES);
        assert_eq!(errno_for_9p_error("parse_failed"), libc::EINVAL);
    }

    #[test]
    fn unknown_remote_error_stays_remote_io() {
        assert_eq!(
            errno_for_9p_error("application-specific gate failed"),
            libc::EREMOTEIO
        );
    }
}
