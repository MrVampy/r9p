use std::{
    fs::File,
    io::{self, BufRead, BufReader, Write},
};

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

pub(crate) fn copy_fid_to_stdout(client: &mut BoxedClient, fid: Fid) -> CliResult<u64> {
    let mut stdout = io::stdout().lock();
    copy_fid_to_writer(client, fid, &mut stdout)
}

pub(crate) fn copy_fid_to_file(
    client: &mut BoxedClient,
    fid: Fid,
    local_path: &str,
) -> CliResult<u64> {
    let mut file = File::create(local_path)?;
    copy_fid_to_writer(client, fid, &mut file)
}

fn copy_fid_to_writer<W: Write>(
    client: &mut BoxedClient,
    fid: Fid,
    writer: &mut W,
) -> CliResult<u64> {
    let mut offset = 0_u64;
    let mut total = 0_u64;
    loop {
        let data = client.read(fid, offset, READ_CHUNK)?;
        if data.is_empty() {
            break;
        }
        let count = u64::try_from(data.len()).map_err(|_| cli_error("read count overflow"))?;
        offset = offset.saturating_add(count);
        total = total.saturating_add(count);
        writer.write_all(&data)?;
    }
    writer.flush()?;
    Ok(total)
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
) -> CliResult<u64> {
    copy_stdin_to_fid_at(client, fid, 0, by_line)
}

pub(crate) fn copy_stdin_to_fid_at(
    client: &mut BoxedClient,
    fid: Fid,
    initial_offset: u64,
    by_line: bool,
) -> CliResult<u64> {
    let mut stdin = io::BufReader::new(io::stdin().lock());
    copy_reader_to_fid_at(client, fid, initial_offset, by_line, &mut stdin)
}

pub(crate) fn copy_file_to_fid_at(
    client: &mut BoxedClient,
    fid: Fid,
    initial_offset: u64,
    local_path: &str,
) -> CliResult<u64> {
    let file = File::open(local_path)?;
    let mut reader = BufReader::new(file);
    copy_reader_to_fid_at(client, fid, initial_offset, false, &mut reader)
}

fn copy_reader_to_fid_at<R: BufRead>(
    client: &mut BoxedClient,
    fid: Fid,
    initial_offset: u64,
    by_line: bool,
    reader: &mut R,
) -> CliResult<u64> {
    let mut offset = initial_offset;
    let mut total = 0_u64;
    let mut wrote = false;
    if by_line {
        loop {
            let mut line = Vec::new();
            let n = reader.read_until(b'\n', &mut line)?;
            if n == 0 {
                break;
            }
            wrote = true;
            write_exact_count(client, fid, offset, &line)?;
            let count = u64::try_from(n).map_err(|_| cli_error("line too large"))?;
            offset = offset.saturating_add(count);
            total = total.saturating_add(count);
        }
    } else {
        let mut buf = [0_u8; 4096];
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            wrote = true;
            write_exact_count(client, fid, offset, &buf[..n])?;
            let count = u64::try_from(n).map_err(|_| cli_error("write too large"))?;
            offset = offset.saturating_add(count);
            total = total.saturating_add(count);
        }
    }
    if !wrote {
        let count = client.write_once(fid, offset, &[])?;
        if count != 0 {
            return Err(cli_error("zero-length write returned non-zero count"));
        }
    }
    Ok(total)
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
