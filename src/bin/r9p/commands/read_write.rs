use r9p::blocking::{OREAD, OTRUNC, OWRITE};

use crate::commands::machine::machine_write_cmd;
use crate::errors::CliResult;
use crate::format::hex_encode;
use crate::io::{
    copy_fid_to_file, copy_fid_to_stdout, copy_file_to_fid_at, copy_stdin_to_fid,
    copy_stdin_to_fid_at, open_path, parse_offset, read_all,
};
use crate::target::{Config, Target};
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
        config,
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
        config,
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

pub(crate) fn write_at_cmd(config: Config, args: Vec<String>) -> CliResult<()> {
    if args.len() != 2 {
        usage();
    }
    let offset = parse_offset(&args[1])?;
    let target = Target {
        config,
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
        config,
        path: args[0].clone(),
    };
    let (mut client, fid) = open_path(&target, OWRITE)?;
    let result = copy_file_to_fid_at(&mut client, fid, offset, &args[2]);
    let clunk = client.clunk(fid);
    let count = result?;
    clunk?;
    println!("write\t{count}");
    Ok(())
}
