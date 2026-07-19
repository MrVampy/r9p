mod config;
mod runtime;

use crate::{errors::CliResult, target::Config};

pub(crate) fn serve_cmd(global: Config, args: Vec<String>) -> CliResult<()> {
    let config = config::parse_serve_config(global, args)?;
    runtime::serve(config)
}

pub(crate) fn export_cmd(global: Config, args: Vec<String>) -> CliResult<()> {
    let config = config::parse_export_config(global, args)?;
    runtime::export(config)
}

#[cfg(test)]
use config::{parse_export_config, parse_serve_config, BindTarget};

#[cfg(test)]
use runtime::required_nofile_limit;

#[cfg(test)]
mod tests;
