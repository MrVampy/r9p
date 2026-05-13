use std::io::{self, BufRead, Read, Write};

use r9p::{
    blocking::{self, BoxedClient},
    fid::Fid,
};

use crate::errors::{cli_error, CliResult};
use crate::target::{target_path, Target};
use crate::transport::dial_target;
use crate::READ_CHUNK;

pub(crate) fn open_path(target: &Target, mode: u8) -> CliResult<(BoxedClient, Fid)> {
    let (mut client, path) = connect_path(target)?;
    let fid = client.walk_path(&path)?;
    client.open(fid, mode)?;
    Ok((client, fid))
}

pub(crate) fn connect_path(target: &Target) -> CliResult<(BoxedClient, String)> {
    let path = target_path(target)?;
    let stream = dial_target(target)?;
    let client = blocking::Client::connect(
        stream,
        &target.config.uname,
        &target.config.aname,
        target.config.msize,
    )?;
    Ok((client, path))
}

pub(crate) fn copy_fid_to_stdout(client: &mut BoxedClient, fid: Fid) -> CliResult<()> {
    let mut stdout = io::stdout().lock();
    let mut offset = 0_u64;
    loop {
        let data = client.read(fid, offset, READ_CHUNK)?;
        if data.is_empty() {
            break;
        }
        offset = offset.saturating_add(
            u64::try_from(data.len()).map_err(|_| cli_error("read count overflow"))?,
        );
        stdout.write_all(&data)?;
    }
    Ok(())
}

pub(crate) fn read_all(client: &mut BoxedClient, fid: Fid) -> CliResult<Vec<u8>> {
    let mut out = Vec::new();
    let mut offset = 0_u64;
    loop {
        let data = client.read(fid, offset, READ_CHUNK)?;
        if data.is_empty() {
            break;
        }
        offset = offset.saturating_add(
            u64::try_from(data.len()).map_err(|_| cli_error("read count overflow"))?,
        );
        out.extend(data);
    }
    Ok(out)
}

pub(crate) fn copy_stdin_to_fid(
    client: &mut BoxedClient,
    fid: Fid,
    by_line: bool,
) -> CliResult<()> {
    copy_stdin_to_fid_at(client, fid, 0, by_line)
}

pub(crate) fn copy_stdin_to_fid_at(
    client: &mut BoxedClient,
    fid: Fid,
    initial_offset: u64,
    by_line: bool,
) -> CliResult<()> {
    let mut stdin = io::BufReader::new(io::stdin().lock());
    let mut offset = initial_offset;
    let mut wrote = false;
    if by_line {
        loop {
            let mut line = Vec::new();
            let n = stdin.read_until(b'\n', &mut line)?;
            if n == 0 {
                break;
            }
            wrote = true;
            write_exact_count(client, fid, offset, &line)?;
            offset =
                offset.saturating_add(u64::try_from(n).map_err(|_| cli_error("line too large"))?);
        }
    } else {
        let mut buf = [0_u8; 4096];
        loop {
            let n = stdin.read(&mut buf)?;
            if n == 0 {
                break;
            }
            wrote = true;
            write_exact_count(client, fid, offset, &buf[..n])?;
            offset =
                offset.saturating_add(u64::try_from(n).map_err(|_| cli_error("write too large"))?);
        }
    }
    if !wrote {
        let count = client.write_once(fid, offset, &[])?;
        if count != 0 {
            return Err(cli_error("zero-length write returned non-zero count"));
        }
    }
    Ok(())
}

pub(crate) fn write_exact_count(
    client: &mut BoxedClient,
    fid: Fid,
    offset: u64,
    data: &[u8],
) -> CliResult<()> {
    let count = client.write(fid, offset, data)?;
    if count as usize != data.len() {
        return Err(cli_error("short write"));
    }
    Ok(())
}

pub(crate) fn parse_offset(value: &str) -> CliResult<u64> {
    value
        .parse()
        .map_err(|_| cli_error(format!("invalid offset {value}")))
}
