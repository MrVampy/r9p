use std::io::{IsTerminal, Read};

use r9p::fid::Fid;
use r9p::{
    blocking::{ORDWR, OREAD, OTRUNC, OWRITE},
    qid::{DMDIR, QTDIR},
    stat::Stat,
};

use crate::commands::machine::machine_write_cmd;
use crate::commands::mutate::split_parent;
use crate::errors::{cli_error, CliResult};
use crate::format::hex_encode;
use crate::io::{
    connect_path, copy_fid_to_file, copy_fid_to_stdout, copy_file_to_fid_at, copy_stdin_to_fid,
    copy_stdin_to_fid_at, open_path, parse_offset, read_all,
};
use crate::target::{operation_config, write_config_for_path, Config, Target};
use crate::usage;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum ReadMode {
    Read,
    ReadFd,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum WriteMode {
    Write,
    WriteFd,
}

pub(crate) fn read_cmd(config: Config, args: Vec<String>, mode: ReadMode) -> CliResult<()> {
    if args.len() != 1 {
        usage();
    }
    let target = Target {
        config,
        path: args[0].clone(),
    };
    let (client, fid) = open_path(&target, OREAD)?;
    let result = if target.config.machine {
        match mode {
            ReadMode::Read => {
                let data = read_all(&client, fid)?;
                println!("read\t{}", hex_encode(&data));
                Ok(())
            }
            ReadMode::ReadFd => copy_fid_to_stdout(&client, fid).map(|_| ()),
        }
    } else {
        copy_fid_to_stdout(&client, fid).map(|_| ())
    };
    let clunk = client.clunk(fid);
    result?;
    clunk?;
    Ok(())
}

pub(crate) fn read_to_cmd(config: Config, args: Vec<String>) -> CliResult<()> {
    if !config.machine || args.len() != 2 {
        usage();
    }
    let target = Target {
        config,
        path: args[0].clone(),
    };
    let (client, fid) = open_path(&target, OREAD)?;
    let result = copy_fid_to_file(&client, fid, &args[1]);
    let clunk = client.clunk(fid);
    let count = result?;
    clunk?;
    println!("read\t{count}");
    Ok(())
}

pub(crate) fn write_cmd(config: Config, mut args: Vec<String>, mode: WriteMode) -> CliResult<()> {
    if config.machine {
        return match mode {
            WriteMode::Write => machine_write_cmd(config, args),
            WriteMode::WriteFd => machine_write_fd_cmd(config, args),
        };
    }
    let by_line = if mode == WriteMode::Write && args.first().is_some_and(|arg| arg == "-l") {
        args.remove(0);
        true
    } else {
        false
    };
    if args.len() != 1 {
        usage();
    }
    let target = Target {
        config: write_config_for_path(config, &args[0]),
        path: args[0].clone(),
    };
    let (client, fid) = open_path(&target, OWRITE | OTRUNC)?;
    let result = copy_stdin_to_fid(&client, fid, by_line);
    let clunk = client.clunk(fid);
    let _count = result?;
    clunk?;
    Ok(())
}

fn machine_write_fd_cmd(config: Config, args: Vec<String>) -> CliResult<()> {
    if args.len() != 1 {
        usage();
    }
    let target = Target {
        config: write_config_for_path(config, &args[0]),
        path: args[0].clone(),
    };
    let (client, fid) = open_path(&target, OWRITE | OTRUNC)?;
    let result = copy_stdin_to_fid(&client, fid, false);
    let clunk = client.clunk(fid);
    let count = result?;
    clunk?;
    println!("write\t{count}");
    Ok(())
}

enum RequestSource {
    Given(Vec<u8>),
    Empty,
    Stdin,
}

fn request_source(argument: Option<&String>, stdin_is_terminal: bool) -> RequestSource {
    match argument {
        Some(request) => RequestSource::Given(request.clone().into_bytes()),
        None if stdin_is_terminal => RequestSource::Empty,
        None => RequestSource::Stdin,
    }
}

pub(crate) fn rpc_cmd(config: Config, args: Vec<String>) -> CliResult<()> {
    if args.is_empty() || args.len() > 2 {
        usage();
    }
    let request = match request_source(args.get(1), std::io::stdin().is_terminal()) {
        RequestSource::Given(request) => request,
        RequestSource::Empty => Vec::new(),
        RequestSource::Stdin => {
            let mut buf = Vec::new();
            std::io::stdin().read_to_end(&mut buf)?;
            buf
        }
    };
    let target = Target {
        config: operation_config(config),
        path: args[0].clone(),
    };
    let (client, fid) = open_rpc_path(&target)?;
    let result = rpc_exchange(&client, fid, &request);
    let clunk = client.clunk(fid);
    result?;
    clunk?;
    Ok(())
}

fn open_rpc_path(target: &Target) -> CliResult<(session::Client, Fid)> {
    let (client, path) = connect_path(target)?;
    let fid = client.walk_path(&path)?;
    let stat = client.stat(fid)?;
    match client.open(fid, ORDWR) {
        Ok(_) => Ok((client, fid)),
        Err(error) => {
            let _ = client.clunk(fid);
            if let Some(hint) = rpc_open_hint(&target.path, &stat) {
                Err(cli_error(format!("{hint}: {error}")))
            } else {
                Err(error.into())
            }
        }
    }
}

fn rpc_open_hint(path: &str, stat: &Stat) -> Option<String> {
    if stat.qid.qtype & QTDIR != 0 || stat.mode & DMDIR != 0 {
        return Some(format!("{path} is a directory; use ls {path}"));
    }
    if stat.mode & 0o222 == 0 {
        return Some(format!("{path} is a read-only file; use read {path}"));
    }
    None
}

fn rpc_exchange(client: &session::Client, fid: Fid, request: &[u8]) -> CliResult<()> {
    let count = client.write(fid, 0, request)?;
    if count as usize != request.len() {
        return Err(cli_error("short rpc request write"));
    }
    copy_fid_to_stdout(client, fid)?;
    Ok(())
}

pub(crate) fn write_at_cmd(config: Config, args: Vec<String>) -> CliResult<()> {
    if args.len() != 2 {
        usage();
    }
    let offset = parse_offset(&args[1])?;
    let target = Target {
        config: write_config_for_path(config, &args[0]),
        path: args[0].clone(),
    };
    let (client, fid) = open_path(&target, OWRITE)?;
    let result = copy_stdin_to_fid_at(&client, fid, offset, false);
    let clunk = client.clunk(fid);
    let count = result?;
    clunk?;
    if target.config.machine {
        println!("write\t{count}");
    }
    Ok(())
}

pub(crate) fn write_from_cmd(config: Config, args: Vec<String>) -> CliResult<()> {
    if !config.machine || args.len() != 3 {
        usage();
    }
    let offset = parse_offset(&args[1])?;
    let target = Target {
        config: write_config_for_path(config, &args[0]),
        path: args[0].clone(),
    };
    let count = write_local_file_to_target(&target, offset, OWRITE, &args[2])?;
    println!("write\t{count}");
    Ok(())
}

pub(crate) fn write_from_trunc_cmd(config: Config, args: Vec<String>) -> CliResult<()> {
    if !config.machine || args.len() != 2 {
        usage();
    }
    let target = Target {
        config: write_config_for_path(config, &args[0]),
        path: args[0].clone(),
    };
    let count = write_local_file_to_target(&target, 0, OWRITE | OTRUNC, &args[1])?;
    println!("write\t{count}");
    Ok(())
}

fn write_local_file_to_target(
    target: &Target,
    offset: u64,
    open_mode: u8,
    local_path: &str,
) -> CliResult<u64> {
    let (client, fid) = open_path(target, open_mode)?;
    let result = copy_file_to_fid_at(&client, fid, offset, local_path);
    let clunk = client.clunk(fid);
    let count = result?;
    clunk?;
    Ok(count)
}

pub(crate) fn create_write_from_cmd(config: Config, args: Vec<String>) -> CliResult<()> {
    if !config.machine || args.len() != 5 {
        usage();
    }
    let perm = args[1]
        .parse::<u32>()
        .map_err(|_| cli_error(format!("invalid perm {}", args[1])))?;
    let mode = args[2]
        .parse::<u8>()
        .map_err(|_| cli_error(format!("invalid mode {}", args[2])))?;
    let offset = parse_offset(&args[3])?;
    let target = Target {
        config: write_config_for_path(config, &args[0]),
        path: args[0].clone(),
    };
    let (parent, name) = split_parent(&target.path)?;
    let parent_target = Target {
        config: target.config.clone(),
        path: parent,
    };
    let (client, path) = connect_path(&parent_target)?;
    let parent_fid = client.walk_path(&path)?;
    let created = client.create(parent_fid, name.as_bytes(), perm, mode);
    let parent_clunk = client.clunk(parent_fid);
    let (fid, _) = created?;
    parent_clunk?;
    let result = copy_file_to_fid_at(&client, fid, offset, &args[4]);
    let clunk = client.clunk(fid);
    let count = result?;
    clunk?;
    println!("write\t{count}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{request_source, rpc_open_hint, RequestSource};
    use r9p::{
        qid::{Qid, DMDIR},
        stat::Stat,
    };

    #[test]
    fn rpc_hint_points_read_only_files_at_read() {
        let stat = Stat::new("status", Qid::file(7), 0o444);
        assert_eq!(
            rpc_open_hint("/sources/x/status", &stat),
            Some("/sources/x/status is a read-only file; use read /sources/x/status".to_string())
        );
    }

    #[test]
    fn rpc_hint_points_directories_at_ls() {
        let stat = Stat::new("sources", Qid::dir(7), DMDIR | 0o555);
        assert_eq!(
            rpc_open_hint("/sources", &stat),
            Some("/sources is a directory; use ls /sources".to_string())
        );
    }

    #[test]
    fn rpc_hint_leaves_writeable_files_to_protocol_errors() {
        let stat = Stat::new("run", Qid::file(7), 0o600);
        assert_eq!(rpc_open_hint("/operations/srvcheck/run", &stat), None);
    }

    #[test]
    fn an_omitted_request_at_a_terminal_is_empty_rather_than_a_read() {
        assert!(matches!(request_source(None, true), RequestSource::Empty));
    }

    #[test]
    fn an_omitted_request_off_a_terminal_still_comes_from_stdin() {
        assert!(matches!(request_source(None, false), RequestSource::Stdin));
    }

    #[test]
    fn a_given_request_is_used_whatever_stdin_is() {
        let request = "{}".to_string();
        for terminal in [true, false] {
            match request_source(Some(&request), terminal) {
                RequestSource::Given(bytes) => assert_eq!(bytes, b"{}"),
                _ => panic!("a given request must be used"),
            }
        }
    }
}
