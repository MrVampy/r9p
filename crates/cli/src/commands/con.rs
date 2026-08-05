use std::{
    io::{self, Read, Write},
    thread,
    time::Duration,
};

use session::{Client, ClientSession, ConnectionConfig, ResumableFid, ORDWR};

use crate::errors::{cli_error, CliResult};
use crate::target::{split_namespace_path, target_path, Config, Target};
use crate::usage;
use crate::{CTRL_R, READ_CHUNK};

pub(crate) fn con_cmd(config: Config, mut args: Vec<String>) -> CliResult<()> {
    let mut strip_cr = true;
    let mut resume = false;
    args.retain(|arg| match arg.as_str() {
        "-r" => {
            strip_cr = false;
            false
        }
        "--resume" => {
            resume = true;
            false
        }
        _ => true,
    });
    if args.len() != 1 {
        usage();
    }
    let target = Target {
        config,
        path: args[0].clone(),
    };
    if resume {
        return resumable_con(&target, strip_cr);
    }
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

fn resumable_con(target: &Target, strip_cr: bool) -> CliResult<()> {
    let (config, path, timeout) = stream_target(target)?;
    let session = ClientSession::connect(&config, timeout)?;
    let mut reader = ResumableFid::open(session.clone(), &path, ORDWR, timeout)?;
    let writer = ResumableFid::open(session.clone(), path, ORDWR, timeout)?;
    let writer_session = session.clone();
    thread::spawn(move || {
        if let Err(error) = resumable_con_writer(writer) {
            eprintln!("write: {error}");
            let _ = writer_session.shutdown();
        }
    });

    let mut stdout = io::stdout().lock();
    let mut offset = 0_u64;
    loop {
        let mut data = reader.read(offset, READ_CHUNK)?;
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
    reader.close()?;
    session.shutdown()?;
    Ok(())
}

fn open_stream(target: &Target) -> CliResult<(Client, r9p::fid::Fid, r9p::fid::Fid)> {
    let (config, path, timeout) = stream_target(target)?;
    let client = Client::connect_with_timeout(&config, timeout)?;
    let reader_fid = client.walk_path(&path)?;
    client.open(reader_fid, ORDWR)?;
    let writer_fid = client.walk_path(&path)?;
    client.open(writer_fid, ORDWR)?;
    Ok((client, reader_fid, writer_fid))
}

fn stream_target(target: &Target) -> CliResult<(ConnectionConfig, String, Duration)> {
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
    let timeout = target.config.request_timeout.unwrap_or(Duration::ZERO);
    Ok((
        ConnectionConfig {
            auth_domain: target.config.auth_domain.clone(),
            address,
            uname: target.config.uname.clone(),
            aname: target.config.aname.clone(),
            msize: target.config.msize,
            auth_config: target.config.auth_config.clone(),
            authorities: target.config.authorities.clone(),
        },
        path,
        timeout,
    ))
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

fn resumable_con_writer(mut stream: ResumableFid) -> CliResult<()> {
    let mut stdin = io::stdin().lock();
    let mut offset = 0_u64;
    let mut buf = [0_u8; 4096];
    loop {
        let n = stdin.read(&mut buf)?;
        if n == 0 || buf[0] == CTRL_R {
            break;
        }
        let count = stream.write(offset, &buf[..n])?;
        if usize::try_from(count).ok() != Some(n) {
            return Err(cli_error("short write"));
        }
        offset = offset.saturating_add(u64::from(count));
    }
    stream.close()?;
    Ok(())
}
