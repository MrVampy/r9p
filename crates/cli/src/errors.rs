use std::{error::Error, io};

pub type CliResult<T> = Result<T, Box<dyn Error>>;

pub fn cli_error(message: impl Into<String>) -> Box<dyn Error> {
    Box::new(io::Error::other(message.into()))
}
