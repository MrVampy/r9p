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

impl From<session::Error> for Error {
    fn from(error: session::Error) -> Self {
        Self::new(error.errno, error.message().to_string())
    }
}
