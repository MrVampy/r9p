use r9p::{
    blocking::{BoxedClient, OREAD},
    fid::Fid,
    qid::DMDIR,
    stat::{decode_dir_entries, Stat},
};

use crate::errors::{cli_error, CliResult};
use crate::format::{mode_string, quote_name, text, time_string};
use crate::io::connect_path;
use crate::target::{Config, Target};
use crate::READ_CHUNK;

#[derive(Debug)]
pub(crate) struct LsOptions {
    pub(crate) long: bool,
    pub(crate) directory: bool,
    pub(crate) no_sort: bool,
    pub(crate) sort_time: bool,
}

pub(crate) fn ls_cmd(config: Config, mut args: Vec<String>) -> CliResult<()> {
    let options = parse_ls_options(&mut args)?;
    if args.is_empty() {
        args.push(".".to_string());
    }
    let mut had_error = false;
    for path in args {
        let target = Target {
            config: config.clone(),
            path: path.clone(),
        };
        if let Err(error) = ls_one(&target, &options) {
            eprintln!("ls {path}: {error}");
            had_error = true;
        }
    }
    if had_error {
        return Err(cli_error("ls errors"));
    }
    Ok(())
}

pub(crate) fn parse_ls_options(args: &mut Vec<String>) -> CliResult<LsOptions> {
    let mut options = LsOptions {
        long: false,
        directory: false,
        no_sort: false,
        sort_time: false,
    };
    let mut rest = Vec::new();
    let mut parsing = true;
    for arg in args.drain(..) {
        if parsing && arg == "--" {
            parsing = false;
            continue;
        }
        if parsing && arg.starts_with('-') && arg != "-" {
            for flag in arg[1..].chars() {
                match flag {
                    'l' => options.long = true,
                    'd' => options.directory = true,
                    'n' => options.no_sort = true,
                    't' => options.sort_time = true,
                    _ => return Err(cli_error(format!("unknown ls option -{flag}"))),
                }
            }
        } else {
            rest.push(arg);
        }
    }
    *args = rest;
    Ok(options)
}

pub(crate) fn ls_one(target: &Target, options: &LsOptions) -> CliResult<()> {
    let (mut client, path) = connect_path(target)?;
    let fid = client.walk_path(&path)?;
    let stat = client.stat(fid)?;
    if stat.mode & DMDIR != 0 && !options.directory {
        client.open(fid, OREAD)?;
        let mut stats = read_dir_stats(&mut client, fid)?;
        if !options.no_sort {
            if options.sort_time {
                stats.sort_by_key(|stat| stat.mtime);
            } else {
                stats.sort_by(|left, right| left.name.cmp(&right.name));
            }
        }
        print_ls_entries(&stats, options.long);
    } else {
        print_ls_entries(&[stat], options.long);
    }
    client.clunk(fid)?;
    Ok(())
}

pub(crate) fn read_dir_stats(client: &mut BoxedClient, fid: Fid) -> CliResult<Vec<Stat>> {
    let mut offset = 0_u64;
    let mut data = Vec::new();
    loop {
        let chunk = client.read(fid, offset, READ_CHUNK)?;
        if chunk.is_empty() {
            break;
        }
        offset = offset.saturating_add(
            u64::try_from(chunk.len()).map_err(|_| cli_error("directory read overflow"))?,
        );
        data.extend(chunk);
    }
    Ok(decode_dir_entries(&data)?)
}

pub(crate) fn print_ls_entries(stats: &[Stat], long: bool) {
    if !long {
        for stat in stats {
            println!("{}", quote_name(&stat.name));
        }
        return;
    }
    let widths = LsWidths::from_stats(stats);
    for stat in stats {
        let uid = text(&stat.uid);
        let gid = text(&stat.gid);
        println!(
            "{} M {:>dev_width$} {:<uid_width$} {:<gid_width$} {:>len_width$} {} {}",
            mode_string(stat.mode),
            stat.dev,
            uid,
            gid,
            stat.length,
            time_string(stat.mtime),
            quote_name(&stat.name),
            dev_width = widths.dev,
            uid_width = widths.uid,
            gid_width = widths.gid,
            len_width = widths.len,
        );
    }
}

#[derive(Debug)]
pub(crate) struct LsWidths {
    pub(crate) dev: usize,
    pub(crate) uid: usize,
    pub(crate) gid: usize,
    pub(crate) len: usize,
}

impl LsWidths {
    pub(crate) fn from_stats(stats: &[Stat]) -> Self {
        let mut widths = Self {
            dev: 1,
            uid: 1,
            gid: 1,
            len: 1,
        };
        for stat in stats {
            widths.dev = widths.dev.max(stat.dev.to_string().len());
            widths.uid = widths.uid.max(text(&stat.uid).len());
            widths.gid = widths.gid.max(text(&stat.gid).len());
            widths.len = widths.len.max(stat.length.to_string().len());
        }
        widths
    }
}
