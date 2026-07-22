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
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({})", self.message, self.errno)
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

pub(crate) fn client_error(error: r9p::Error) -> Error {
    let message = error.display_lossy().to_string();
    if is_protocol_error(&message) {
        Error::new(libc::EPROTO, format!("9P client state: {message}"))
    } else if message.contains("zero-length 9P write progress") {
        Error::new(libc::EIO, format!("9P client state: {message}"))
    } else if message.contains("write count overflow") {
        Error::new(libc::EOVERFLOW, format!("9P client state: {message}"))
    } else if is_transport_message(&message) {
        Error::new(
            transport_errno(&message).unwrap_or(libc::EIO),
            format!("9P client state: {message}"),
        )
    } else {
        p9_error(error.message())
    }
}

// 9P2000 Rerror carries text, not numeric errno. Linux-facing projections still
// need errno-shaped failures, and non-FUSE session consumers also benefit from a
// stable machine-readable error class.
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
    // Never expose remote "not implemented" as FUSE ENOSYS. Linux caches
    // ENOSYS per opcode for the mount lifetime; a backend rejection for one
    // operation must not brick that FUSE opcode until remount.
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
    ("not open", libc::EBADF),
    ("already open", libc::EBUSY),
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

fn is_protocol_error(message: &str) -> bool {
    message.starts_with("9P client state:")
        || message.starts_with("response tag mismatch")
        || message.starts_with("unknown response")
        || message.starts_with("duplicate waiter")
        || message.starts_with("multiplexed calls require")
        || message.starts_with("expected ")
        || message.contains("reported more bytes written than requested")
}

fn is_transport_message(message: &str) -> bool {
    (message.starts_with("connect ") && message.contains("(os error "))
        || message.contains("9P frame")
        || message.contains("9P reader stopped")
        || message.contains("9P response timeout")
        || message.contains("clone 9P stream")
        || message.contains("lock 9P")
}

fn transport_errno(message: &str) -> Option<i32> {
    if message.contains("9P reader stopped") {
        return Some(libc::ENOTCONN);
    }
    if message.contains("9P response timeout") {
        return Some(libc::ETIMEDOUT);
    }
    let marker = "os error ";
    let start = message.find(marker)? + marker.len();
    let rest = &message[start..];
    let digits = rest
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .collect::<String>();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::{client_error, errno_for_9p_error};
    use r9p::error::Error as P9Error;

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
        assert_eq!(errno_for_9p_error("fid not open"), libc::EBADF);
        assert_eq!(errno_for_9p_error("fid already open"), libc::EBUSY);
        assert_eq!(errno_for_9p_error("fid busy"), libc::EBUSY);
    }

    #[test]
    fn unknown_remote_error_stays_remote_io() {
        assert_eq!(
            errno_for_9p_error("application-specific gate failed"),
            libc::EREMOTEIO
        );
    }

    #[test]
    fn closed_multiplex_reader_maps_to_transport_errno() {
        let error = client_error(P9Error::from("9P reader stopped before response"));
        assert_eq!(error.errno, libc::ENOTCONN);
        assert!(error.message().contains("9P reader stopped"));
    }

    #[test]
    fn local_connection_refusal_maps_to_transport_errno() {
        let error = client_error(P9Error::from(
            "connect 127.0.0.1:9641: Connection refused (os error 111)",
        ));
        assert_eq!(error.errno, libc::ECONNREFUSED);
        assert!(error.message().contains("Connection refused"));
    }
}
