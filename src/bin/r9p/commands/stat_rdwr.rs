use std::io::{self, BufRead, Write};

use r9p::blocking::ORDWR;

use crate::commands::machine::print_machine_stat;
use crate::errors::{cli_error, CliResult};
use crate::format::format_stat;
use crate::io::{connect_path, open_path};
use crate::target::{Config, Target};
use crate::usage;
use crate::READ_CHUNK;

pub(crate) fn stat_cmd(config: Config, args: Vec<String>) -> CliResult<()> {
    if args.len() != 1 {
        usage();
    }
    let target = Target {
        config,
        path: args[0].clone(),
    };
    let (mut client, path) = connect_path(&target)?;
    let fid = client.walk_path(&path)?;
    let stat = client.stat(fid)?;
    if target.config.machine {
        print_machine_stat("stat", &stat);
    } else {
        println!("{}", format_stat(&stat));
    }
    client.clunk(fid)?;
    Ok(())
}

pub(crate) fn rdwr_cmd(config: Config, args: Vec<String>) -> CliResult<()> {
    if args.len() != 1 {
        usage();
    }
    let target = Target {
        config,
        path: args[0].clone(),
    };
    let (mut client, fid) = open_path(&target, ORDWR)?;
    let mut stdin = io::BufReader::new(io::stdin().lock());
    let mut stdout = io::stdout().lock();
    loop {
        let data = client.read(fid, 0, READ_CHUNK).map_err(|error| {
            eprintln!("read: {error}");
            error
        });
        let write_offset = match data {
            Ok(data) => {
                stdout.write_all(&data)?;
                stdout.write_all(b"\n")?;
                u64::try_from(data.len()).map_err(|_| cli_error("read count overflow"))?
            }
            Err(_) => 0,
        };

        let mut line = Vec::new();
        if stdin.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        let count = client.write(fid, write_offset, &line);
        match count {
            Ok(count) if count as usize == line.len() => {}
            Ok(_) => eprintln!("write: short write"),
            Err(error) => eprintln!("write: {error}"),
        }
    }
    client.clunk(fid)?;
    Ok(())
}
