mod config;
mod supervisor;

use crate::{errors::CliResult, target::Config};

use config::parse_mount_config;

pub(crate) fn mount_cmd(global: Config, args: Vec<String>) -> CliResult<()> {
    if let Some(action) = args.first().map(String::as_str) {
        match action {
            "ensure" => return supervisor::mount_ensure_cmd(args[1..].to_vec()),
            "read-ahead" => return supervisor::mount_read_ahead_cmd(args[1..].to_vec()),
            "replace" => {
                let (replacement, mount_args) =
                    supervisor::parse_mount_replacement_config(args[1..].to_vec())?;
                let config = parse_mount_config(global, mount_args)?;
                return supervisor::mount_replace_cmd(replacement, config);
            }
            "status" => return supervisor::mount_status_cmd(args[1..].to_vec()),
            "stop" => return supervisor::mount_stop_cmd(args[1..].to_vec()),
            _ => {}
        }
    }
    let config = parse_mount_config(global, args)?;
    fuse::mount(config)
        .map_err(|error| crate::errors::cli_error(format!("mount: {}", error.message())))
}

#[cfg(test)]
use supervisor::{
    decode_mountinfo_path, mountinfo_targets_for_absolute, parse_mount_ensure_config,
    parse_mount_replacement_config, parse_mount_supervisor_config, systemd_command,
    SystemdUnitScope,
};

#[cfg(test)]
mod tests;
