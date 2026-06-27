use r9p::{
    blocking::{ORDWR, OREAD, OWRITE},
    codec,
    qid::Qid,
    qid::DMDIR,
    stat::Stat,
};

use crate::commands::ls::read_dir_stats;
use crate::commands::mutate::{remove_one, split_parent};
use crate::errors::{cli_error, CliResult};
use crate::format::{hex_decode, hex_encode};
use crate::io::{connect_path, open_path, parse_offset, read_all, write_exact_count};
use crate::target::{operation_config, write_config_for_path, Config, Target};
use crate::usage;

pub(crate) fn machine_list_cmd(config: Config, args: Vec<String>) -> CliResult<()> {
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
    if stat.mode & DMDIR != 0 {
        client.open(fid, OREAD)?;
        let stats = read_dir_stats(&mut client, fid)?;
        for stat in stats {
            print_machine_stat("entry", &stat);
        }
    } else {
        print_machine_stat("entry", &stat);
    }
    client.clunk(fid)?;
    Ok(())
}

pub(crate) fn machine_write_cmd(config: Config, args: Vec<String>) -> CliResult<()> {
    if args.len() != 3 {
        usage();
    }
    let offset = parse_offset(&args[1])?;
    let data = hex_decode(&args[2])?;
    let target = Target {
        config: write_config_for_path(config, &args[0]),
        path: args[0].clone(),
    };
    let (mut client, fid) = open_path(&target, OWRITE)?;
    let count = client.write(fid, offset, &data)?;
    client.clunk(fid)?;
    println!("write\t{count}");
    Ok(())
}

pub(crate) fn machine_rpc_hex_cmd(config: Config, args: Vec<String>) -> CliResult<()> {
    if args.len() != 2 {
        usage();
    }
    let request = hex_decode(&args[1])?;
    let target = Target {
        config: operation_config(config),
        path: args[0].clone(),
    };
    let (mut client, fid) = open_path(&target, ORDWR)?;
    let result = (|| {
        write_exact_count(&mut client, fid, 0, &request)?;
        read_all(&mut client, fid)
    })();
    let clunk = client.clunk(fid);
    let response = result?;
    clunk?;
    println!("rpc\t{}\t{}", response.len(), hex_encode(&response));
    Ok(())
}

pub(crate) fn machine_create_cmd(config: Config, args: Vec<String>) -> CliResult<()> {
    if args.len() != 3 {
        usage();
    }
    let perm = args[1]
        .parse::<u32>()
        .map_err(|_| cli_error(format!("invalid perm {}", args[1])))?;
    let mode = args[2]
        .parse::<u8>()
        .map_err(|_| cli_error(format!("invalid mode {}", args[2])))?;
    let target = Target {
        config,
        path: args[0].clone(),
    };
    let (parent, name) = split_parent(&target.path)?;
    let parent_target = Target {
        config: target.config.clone(),
        path: parent,
    };
    let (mut client, path) = connect_path(&parent_target)?;
    let parent_fid = client.walk_path(&path)?;
    let (fid, qid) = client.create(parent_fid, name.as_bytes(), perm, mode)?;
    let iounit = codec::max_write_payload(client.msize());
    println!(
        "create\t{}\t{}\t{}\t{}",
        qid.qtype, qid.version, qid.path, iounit
    );
    client.clunk(fid)?;
    client.clunk(parent_fid)?;
    Ok(())
}

pub(crate) fn machine_remove_cmd(config: Config, args: Vec<String>) -> CliResult<()> {
    if args.len() != 1 {
        usage();
    }
    let target = Target {
        config,
        path: args[0].clone(),
    };
    remove_one(&target)?;
    println!("ok");
    Ok(())
}

pub(crate) fn print_machine_qid(prefix: &str, qid: Qid) {
    println!("{}\t{}\t{}\t{}", prefix, qid.qtype, qid.version, qid.path);
}

pub(crate) fn print_machine_stat(prefix: &str, stat: &Stat) {
    println!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        prefix,
        hex_encode(&stat.name),
        stat.qid.qtype,
        stat.qid.version,
        stat.qid.path,
        stat.length,
        stat.mode,
        stat.atime,
        stat.mtime,
        stat.type_,
        stat.dev,
        hex_encode(&stat.uid),
        hex_encode(&stat.gid),
        hex_encode(&stat.muid),
    );
}
