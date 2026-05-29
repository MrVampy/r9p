use std::{
    fs::File,
    io::{BufRead, BufReader},
};

use r9p::{
    blocking::{BoxedClient, OREAD, OTRUNC, OWRITE},
    fid::Fid,
    qid::DMDIR,
};

use crate::commands::mutate::split_parent;
use crate::errors::{cli_error, CliResult};
use crate::format::hex_encode;
use crate::io::{connect_path, copy_fid_to_file, copy_file_to_fid_at, parse_offset};
use crate::target::{Config, Target};
use crate::{usage, READ_CHUNK};

pub(crate) fn machine_script_cmd(config: Config, args: Vec<String>) -> CliResult<()> {
    if !config.machine {
        usage();
    }
    let (service, script_path) = match (config.address.is_some(), args.as_slice()) {
        (true, [script_path]) => ("/".to_string(), script_path.clone()),
        (false, [service, script_path]) => (service.clone(), script_path.clone()),
        _ => usage(),
    };
    let target = Target {
        config,
        path: service,
    };
    let (mut client, _) = connect_path(&target)?;
    let script = File::open(&script_path)?;
    for (index, line) in BufReader::new(script).lines().enumerate() {
        let line_number = index + 1;
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        run_script_line(&target, &mut client, line_number, &line)?;
    }
    Ok(())
}

fn run_script_line(
    target: &Target,
    client: &mut BoxedClient,
    line_number: usize,
    line: &str,
) -> CliResult<()> {
    let fields = line.split('\t').collect::<Vec<_>>();
    match fields.as_slice() {
        ["write-hex", path, offset, payload_hex] => {
            let offset = parse_offset(offset)?;
            let data = crate::format::hex_decode(payload_hex)?;
            let fid = walk_open(client, path, OWRITE)?;
            let result = client.write(fid, offset, &data);
            let clunk = client.clunk(fid);
            let count = result?;
            clunk?;
            println!("ok\t{line_number}\twrite\t{count}");
        }
        ["write-from", path, offset, local_path] => {
            let offset = parse_offset(offset)?;
            let fid = walk_open(client, path, OWRITE)?;
            let result = copy_file_to_fid_at(client, fid, offset, local_path);
            let clunk = client.clunk(fid);
            let count = result?;
            clunk?;
            println!("ok\t{line_number}\twrite\t{count}");
        }
        ["write-from-trunc", path, local_path] => {
            let fid = walk_open(client, path, OWRITE | OTRUNC)?;
            let result = copy_file_to_fid_at(client, fid, 0, local_path);
            let clunk = client.clunk(fid);
            let count = result?;
            clunk?;
            println!("ok\t{line_number}\twrite\t{count}");
        }
        ["create-write-from", path, perm, mode, offset, local_path] => {
            let offset = parse_offset(offset)?;
            let count = create_write_from_path(
                client,
                path,
                parse_perm(perm)?,
                parse_mode(mode)?,
                offset,
                local_path,
            )?;
            println!("ok\t{line_number}\tcreate-write\t{count}");
        }
        ["create", path] => {
            create_path(client, path, 0o666, OREAD)?;
            println!("ok\t{line_number}\tcreate");
        }
        ["create", path, perm, mode] => {
            create_path(client, path, parse_perm(perm)?, parse_mode(mode)?)?;
            println!("ok\t{line_number}\tcreate");
        }
        ["mkdir", path] => {
            create_path(client, path, DMDIR | 0o755, OREAD)?;
            println!("ok\t{line_number}\tmkdir");
        }
        ["mkdir", path, perm, mode] => {
            create_path(client, path, DMDIR | parse_perm(perm)?, parse_mode(mode)?)?;
            println!("ok\t{line_number}\tmkdir");
        }
        ["read-to", path, local_path] => {
            let fid = walk_open(client, path, OREAD)?;
            let result = copy_fid_to_file(client, fid, local_path);
            let clunk = client.clunk(fid);
            let count = result?;
            clunk?;
            println!("ok\t{line_number}\tread\t{count}");
        }
        ["read-hex", path, offset, count] => {
            let offset = parse_offset(offset)?;
            let count = parse_count(count)?;
            let fid = walk_open(client, path, OREAD)?;
            let result = read_range(client, fid, offset, count);
            let clunk = client.clunk(fid);
            let data = result?;
            clunk?;
            println!(
                "ok\t{line_number}\tread-hex\t{}\t{}",
                data.len(),
                hex_encode(&data)
            );
        }
        ["fresh-stat-error", path] => {
            fresh_stat_error(target, path, line_number)?;
        }
        [op, ..] => {
            return Err(cli_error(format!(
                "script line {line_number}: unknown operation {op}"
            )));
        }
        [] => {}
    }
    Ok(())
}

fn create_write_from_path(
    client: &mut BoxedClient,
    path: &str,
    perm: u32,
    mode: u8,
    offset: u64,
    local_path: &str,
) -> CliResult<u64> {
    let (parent, name) = split_parent(path)?;
    let parent_fid = client.walk_path(&parent)?;
    let created = client.create(parent_fid, name.as_bytes(), perm, mode);
    let parent_clunk = client.clunk(parent_fid);
    let (fid, _) = created?;
    parent_clunk?;
    let result = copy_file_to_fid_at(client, fid, offset, local_path);
    let created_clunk = client.clunk(fid);
    let count = result?;
    created_clunk?;
    Ok(count)
}

fn create_path(client: &mut BoxedClient, path: &str, perm: u32, mode: u8) -> CliResult<()> {
    let (parent, name) = split_parent(path)?;
    let parent_fid = client.walk_path(&parent)?;
    let created = client.create(parent_fid, name.as_bytes(), perm, mode);
    let parent_clunk = client.clunk(parent_fid);
    let (fid, _) = created?;
    let created_clunk = client.clunk(fid);
    parent_clunk?;
    created_clunk?;
    Ok(())
}

fn parse_perm(value: &str) -> CliResult<u32> {
    u32::from_str_radix(value.trim_start_matches("0o"), 8)
        .or_else(|_| value.parse::<u32>())
        .map_err(|_| cli_error(format!("invalid perm {value}")))
}

fn parse_mode(value: &str) -> CliResult<u8> {
    u8::from_str_radix(value.trim_start_matches("0o"), 8)
        .or_else(|_| value.parse::<u8>())
        .map_err(|_| cli_error(format!("invalid mode {value}")))
}

fn fresh_stat_error(target: &Target, path: &str, line_number: usize) -> CliResult<()> {
    let (mut fresh, _) = connect_path(target)?;
    let result = match fresh.walk_path(path) {
        Ok(fid) => {
            let stat = fresh.stat(fid).map(|_| ());
            let _ = fresh.clunk(fid);
            stat
        }
        Err(error) => Err(error),
    };
    match result {
        Ok(()) => Err(cli_error(format!(
            "script line {line_number}: fresh stat unexpectedly succeeded for {path}"
        ))),
        Err(_) => {
            println!("ok\t{line_number}\tfresh-stat-error");
            Ok(())
        }
    }
}

fn walk_open(client: &mut BoxedClient, path: &str, mode: u8) -> CliResult<Fid> {
    let fid = client.walk_path(path)?;
    match client.open(fid, mode) {
        Ok(_) => Ok(fid),
        Err(error) => {
            let _ = client.clunk(fid);
            Err(error.into())
        }
    }
}

fn read_range(
    client: &mut BoxedClient,
    fid: Fid,
    initial_offset: u64,
    requested: u64,
) -> CliResult<Vec<u8>> {
    let mut out = Vec::new();
    let mut offset = initial_offset;
    let mut remaining = requested;
    while remaining > 0 {
        let count = remaining.min(u64::from(READ_CHUNK));
        let count = u32::try_from(count).map_err(|_| cli_error("read count overflow"))?;
        let data = client.read(fid, offset, count)?;
        if data.is_empty() {
            break;
        }
        let read = u64::try_from(data.len()).map_err(|_| cli_error("read count overflow"))?;
        offset = offset.saturating_add(read);
        remaining = remaining.saturating_sub(read);
        out.extend(data);
    }
    Ok(out)
}

fn parse_count(value: &str) -> CliResult<u64> {
    value
        .parse()
        .map_err(|_| cli_error(format!("invalid count {value}")))
}
