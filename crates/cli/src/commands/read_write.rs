use std::io::Read;

use r9p::blocking::{BoxedClient, ORDWR, OREAD, OTRUNC, OWRITE};
use r9p::fid::Fid;

use crate::commands::machine::machine_write_cmd;
use crate::commands::mutate::split_parent;
use crate::errors::{cli_error, CliResult};
use crate::format::hex_encode;
use crate::io::{
    connect_path, copy_fid_to_file, copy_fid_to_stdout, copy_file_to_fid_at, copy_stdin_to_fid,
    copy_stdin_to_fid_at, open_path, parse_offset, read_all,
};
use crate::target::{write_config_for_path, Config, Target};
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
    let (mut client, fid) = open_path(&target, OREAD)?;
    let result = if target.config.machine {
        match mode {
            ReadMode::Read => {
                let data = read_all(&mut client, fid)?;
                println!("read\t{}", hex_encode(&data));
                Ok(())
            }
            ReadMode::ReadFd => copy_fid_to_stdout(&mut client, fid).map(|_| ()),
        }
    } else {
        copy_fid_to_stdout(&mut client, fid).map(|_| ())
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
    let (mut client, fid) = open_path(&target, OREAD)?;
    let result = copy_fid_to_file(&mut client, fid, &args[1]);
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
    let (mut client, fid) = open_path(&target, OWRITE | OTRUNC)?;
    let result = copy_stdin_to_fid(&mut client, fid, by_line);
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
    let (mut client, fid) = open_path(&target, OWRITE | OTRUNC)?;
    let result = copy_stdin_to_fid(&mut client, fid, false);
    let clunk = client.clunk(fid);
    let count = result?;
    clunk?;
    println!("write\t{count}");
    Ok(())
}

pub(crate) fn rpc_cmd(config: Config, args: Vec<String>) -> CliResult<()> {
    if args.is_empty() || args.len() > 2 {
        usage();
    }
    let request = match args.get(1) {
        Some(request) => request.clone().into_bytes(),
        None => {
            let mut buf = Vec::new();
            std::io::stdin().read_to_end(&mut buf)?;
            buf
        }
    };
    let target = Target {
        config: write_config_for_path(config, &args[0]),
        path: args[0].clone(),
    };
    let (mut client, fid) = open_path(&target, ORDWR)?;
    let result = rpc_exchange(&mut client, fid, &request);
    let clunk = client.clunk(fid);
    result?;
    clunk?;
    Ok(())
}

fn rpc_exchange(client: &mut BoxedClient, fid: Fid, request: &[u8]) -> CliResult<()> {
    let count = client.write_once(fid, 0, request)?;
    if count as usize != request.len() {
        return Err(cli_error("rpc request exceeded a single 9P message"));
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
    let (mut client, fid) = open_path(&target, OWRITE)?;
    let result = copy_stdin_to_fid_at(&mut client, fid, offset, false);
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
    let (mut client, fid) = open_path(target, open_mode)?;
    let result = copy_file_to_fid_at(&mut client, fid, offset, local_path);
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
    let (mut client, path) = connect_path(&parent_target)?;
    let parent_fid = client.walk_path(&path)?;
    let created = client.create(parent_fid, name.as_bytes(), perm, mode);
    let parent_clunk = client.clunk(parent_fid);
    let (fid, _) = created?;
    parent_clunk?;
    let result = copy_file_to_fid_at(&mut client, fid, offset, &args[4]);
    let clunk = client.clunk(fid);
    let count = result?;
    clunk?;
    println!("write\t{count}");
    Ok(())
}
