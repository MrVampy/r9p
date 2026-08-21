mod config;
mod handler;
mod runtime;

use crate::{errors::CliResult, target::Config};

pub(crate) fn stream_export_cmd(global: Config, args: Vec<String>) -> CliResult<()> {
    runtime::run(config::parse(global, args)?)
}

#[cfg(test)]
mod tests;
