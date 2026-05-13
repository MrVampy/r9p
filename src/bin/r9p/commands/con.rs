use std::{
    io::{self, Read, Write},
    thread,
};

use r9p::blocking::ORDWR;

use crate::errors::{cli_error, CliResult};
use crate::io::open_path;
use crate::target::{Config, Target};
use crate::usage;
use crate::{CTRL_R, READ_CHUNK};

pub(crate) fn con_cmd(config: Config, mut args: Vec<String>) -> CliResult<()> {
    let strip_cr = if args.first().is_some_and(|arg| arg == "-r") {
        args.remove(0);
        false
    } else {
        true
    };
    if args.len() != 1 {
        usage();
    }
    let target = Target {
        config,
        path: args[0].clone(),
    };
    let writer_target = target.clone();
    thread::spawn(move || {
        if let Err(error) = con_writer(writer_target) {
            eprintln!("write: {error}");
        }
    });

    let (mut client, fid) = open_path(&target, ORDWR)?;
    let mut stdout = io::stdout().lock();
    let mut offset = 0_u64;
    loop {
        let mut data = client.read(fid, offset, READ_CHUNK)?;
        if data.is_empty() {
            break;
        }
        offset = offset.saturating_add(
            u64::try_from(data.len()).map_err(|_| cli_error("read count overflow"))?,
        );
        if strip_cr {
            data.retain(|byte| *byte != b'\r');
        }
        stdout.write_all(&data)?;
        stdout.flush()?;
    }
    client.clunk(fid)?;
    Ok(())
}

pub(crate) fn con_writer(target: Target) -> CliResult<()> {
    let (mut client, fid) = open_path(&target, ORDWR)?;
    let mut stdin = io::stdin().lock();
    let mut offset = 0_u64;
    let mut buf = [0_u8; 4096];
    loop {
        let n = stdin.read(&mut buf)?;
        if n == 0 || buf[0] == CTRL_R {
            break;
        }
        let count = client.write(fid, offset, &buf[..n])?;
        offset = offset.saturating_add(u64::from(count));
    }
    client.clunk(fid)?;
    Ok(())
}
