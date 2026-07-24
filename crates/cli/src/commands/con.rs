use std::{
    io::{self, Read, Write},
    thread,
    time::Duration,
};

use session::{Client, ConnectionConfig, ORDWR};

use crate::errors::{cli_error, CliResult};
use crate::target::{split_namespace_path, target_path, Config, Target};
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
    let (client, reader_fid, writer_fid) = open_stream(&target)?;
    let writer_client = client.clone();
    thread::spawn(move || {
        if let Err(error) = con_writer(writer_client, writer_fid) {
            eprintln!("write: {error}");
        }
    });

    let mut stdout = io::stdout().lock();
    let mut offset = 0_u64;
    loop {
        let mut data = client.read(reader_fid, offset, READ_CHUNK)?;
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
    client.clunk(reader_fid)?;
    client.shutdown()?;
    Ok(())
}

fn open_stream(target: &Target) -> CliResult<(Client, r9p::fid::Fid, r9p::fid::Fid)> {
    let (address, path) = match &target.config.address {
        Some(address) => (address.clone(), target_path(target)?),
        None => {
            if target.config.auth_config.is_some() {
                return Err(cli_error(
                    "--auth-config requires an endpoint supplied with -a or --bind",
                ));
            }
            let (service, path) = split_namespace_path(&target.path)?;
            (format!("namespace!{service}"), path)
        }
    };
    let client = Client::connect_with_timeout(
        &ConnectionConfig {
            address,
            uname: target.config.uname.clone(),
            aname: target.config.aname.clone(),
            msize: target.config.msize,
            auth_config: target.config.auth_config.clone(),
        },
        target.config.request_timeout.unwrap_or(Duration::ZERO),
    )?;
    let reader_fid = client.walk_path(&path)?;
    client.open(reader_fid, ORDWR)?;
    let writer_fid = client.walk_path(&path)?;
    client.open(writer_fid, ORDWR)?;
    Ok((client, reader_fid, writer_fid))
}

pub(crate) fn con_writer(client: Client, fid: r9p::fid::Fid) -> CliResult<()> {
    let mut stdin = io::stdin().lock();
    let mut offset = 0_u64;
    let mut buf = [0_u8; 4096];
    loop {
        let n = stdin.read(&mut buf)?;
        if n == 0 || buf[0] == CTRL_R {
            break;
        }
        let count = client.write(fid, offset, &buf[..n])?;
        if usize::try_from(count).ok() != Some(n) {
            return Err(cli_error("short write"));
        }
        offset = offset.saturating_add(u64::from(count));
    }
    client.clunk(fid)?;
    Ok(())
}
