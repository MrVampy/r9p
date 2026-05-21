use r9p::{blocking::OREAD, qid::DMDIR};

use crate::commands::machine::machine_create_cmd;
use crate::errors::{cli_error, CliResult};
use crate::io::connect_path;
use crate::target::{Config, Target};
use crate::usage;

pub(crate) fn rm_cmd(config: Config, args: Vec<String>) -> CliResult<()> {
    if args.is_empty() {
        usage();
    }
    let mut had_error = false;
    for path in args {
        let target = Target {
            config: config.clone(),
            path: path.clone(),
        };
        match remove_one(&target) {
            Ok(()) => {}
            Err(error) => {
                eprintln!("remove {path}: {error}");
                had_error = true;
            }
        }
    }
    if had_error {
        return Err(cli_error("remove errors"));
    }
    Ok(())
}

pub(crate) fn remove_one(target: &Target) -> CliResult<()> {
    let (mut client, path) = connect_path(target)?;
    let fid = client.walk_path(&path)?;
    client.remove(fid)?;
    Ok(())
}

pub(crate) fn create_cmd(config: Config, args: Vec<String>) -> CliResult<()> {
    if config.machine {
        return machine_create_cmd(config, args);
    }
    create_paths(config, args, 0o666, OREAD, "create")
}

pub(crate) fn mkdir_cmd(config: Config, args: Vec<String>) -> CliResult<()> {
    create_paths(config, args, DMDIR | 0o755, OREAD, "mkdir")
}

pub(crate) fn create_paths(
    config: Config,
    args: Vec<String>,
    perm: u32,
    mode: u8,
    label: &str,
) -> CliResult<()> {
    if args.is_empty() {
        usage();
    }
    let mut had_error = false;
    for path in args {
        let target = Target {
            config: config.clone(),
            path: path.clone(),
        };
        match create_one(&target, perm, mode) {
            Ok(()) => {}
            Err(error) => {
                eprintln!("{label} {path}: {error}");
                had_error = true;
            }
        }
    }
    if had_error {
        return Err(cli_error(format!("{label} errors")));
    }
    Ok(())
}

pub(crate) fn create_one(target: &Target, perm: u32, mode: u8) -> CliResult<()> {
    if target.config.address.is_none() && !target.path.trim_start_matches('/').contains('/') {
        return Err(cli_error("without -a, create path must be service/name"));
    }
    let (parent, name) = split_parent(&target.path)?;
    let parent_target = Target {
        config: target.config.clone(),
        path: parent,
    };
    let (mut client, path) = connect_path(&parent_target)?;
    let parent_fid = client.walk_path(&path)?;
    let (fid, _) = client.create(parent_fid, name.as_bytes(), perm, mode)?;
    client.clunk(fid)?;
    client.clunk(parent_fid)?;
    Ok(())
}

pub(crate) fn split_parent(path: &str) -> CliResult<(String, String)> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(cli_error("cannot create root"));
    }
    let (parent, name) = match trimmed.rsplit_once('/') {
        Some(("", name)) => ("/".to_string(), name.to_string()),
        Some((parent, name)) => (parent.to_string(), name.to_string()),
        None => (".".to_string(), trimmed.to_string()),
    };
    if name.is_empty() || name == "." || name == ".." {
        return Err(cli_error(format!("bad create name {name}")));
    }
    Ok((parent, name))
}
