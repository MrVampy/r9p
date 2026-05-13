use r9p::blocking::{OREAD, OTRUNC, OWRITE};

use crate::commands::machine::machine_write_cmd;
use crate::errors::CliResult;
use crate::format::hex_encode;
use crate::io::{
    copy_fid_to_stdout, copy_stdin_to_fid, copy_stdin_to_fid_at, open_path, parse_offset, read_all,
};
use crate::target::{Config, Target};
use crate::usage;

pub(crate) fn read_cmd(config: Config, args: Vec<String>) -> CliResult<()> {
    if args.len() != 1 {
        usage();
    }
    let target = Target {
        config,
        path: args[0].clone(),
    };
    let (mut client, fid) = open_path(&target, OREAD)?;
    let result = if target.config.machine {
        let data = read_all(&mut client, fid)?;
        println!("read\t{}", hex_encode(&data));
        Ok(())
    } else {
        copy_fid_to_stdout(&mut client, fid)
    };
    let clunk = client.clunk(fid);
    result?;
    clunk?;
    Ok(())
}

pub(crate) fn write_cmd(
    config: Config,
    mut args: Vec<String>,
    allow_line_option: bool,
) -> CliResult<()> {
    if config.machine {
        return machine_write_cmd(config, args);
    }
    let by_line = if allow_line_option && args.first().is_some_and(|arg| arg == "-l") {
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
    result?;
    clunk?;
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
    result?;
    clunk?;
    Ok(())
}
