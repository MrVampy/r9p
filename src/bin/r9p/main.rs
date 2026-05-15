use std::env;

mod commands;
mod errors;
mod format;
mod io;
mod target;
mod transport;

use commands::{
    con::con_cmd,
    ls::ls_cmd,
    machine::{machine_list_cmd, machine_remove_cmd},
    mutate::{create_cmd, mkdir_cmd, rm_cmd},
    read_write::{
        read_cmd, read_to_cmd, write_at_cmd, write_cmd, write_from_cmd, ReadMode, WriteMode,
    },
    stat_rdwr::{rdwr_cmd, stat_cmd},
    version_attach::{attach_cmd, version_cmd},
};
use errors::{cli_error, CliResult};
use target::Config;

pub(crate) const DEFAULT_MSIZE: u32 = 65_536;
pub(crate) const READ_CHUNK: u32 = 65_536;
pub(crate) const CTRL_R: u8 = b'R' - b'A' + 1;

pub(crate) const DMEXCL: u32 = 0x2000_0000;
pub(crate) const DMAUTH: u32 = 0x0800_0000;
pub(crate) const DMSYMLINK: u32 = 0x0200_0000;
pub(crate) const DMDEVICE: u32 = 0x0080_0000;
pub(crate) const DMNAMEDPIPE: u32 = 0x0020_0000;
pub(crate) const DMSOCKET: u32 = 0x0010_0000;

fn main() {
    if let Err(error) = run() {
        eprintln!("r9p: {error}");
        std::process::exit(1);
    }
}

fn run() -> CliResult<()> {
    let mut args = env::args().skip(1).collect::<Vec<_>>();
    let config = parse_global_options(&mut args)?;
    if args.is_empty() {
        usage();
    }
    let command = args.remove(0);
    match command.as_str() {
        "version" => version_cmd(config, args),
        "attach" => attach_cmd(config, args),
        "read" => read_cmd(config, args, ReadMode::Read),
        "readfd" => read_cmd(config, args, ReadMode::ReadFd),
        "read-to" if config.machine => read_to_cmd(config, args),
        "write" => write_cmd(config, args, WriteMode::Write),
        "write-at" => write_at_cmd(config, args),
        "writefd" => write_cmd(config, args, WriteMode::WriteFd),
        "write-from" if config.machine => write_from_cmd(config, args),
        "stat" => stat_cmd(config, args),
        "rdwr" => rdwr_cmd(config, args),
        "ls" => ls_cmd(config, args),
        "list" if config.machine => machine_list_cmd(config, args),
        "rm" => rm_cmd(config, args),
        "remove" if config.machine => machine_remove_cmd(config, args),
        "create" => create_cmd(config, args),
        "mkdir" => mkdir_cmd(config, args),
        "con" => con_cmd(config, args),
        _ => {
            usage();
        }
    }
}

pub(crate) fn parse_global_options(args: &mut Vec<String>) -> CliResult<Config> {
    let mut config = Config {
        address: None,
        aname: String::new(),
        uname: env::var("USER").unwrap_or_else(|_| "none".to_string()),
        msize: DEFAULT_MSIZE,
        machine: false,
    };
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            rest.extend(args[i + 1..].iter().cloned());
            break;
        }
        if !arg.starts_with('-') || arg == "-" {
            rest.extend(args[i..].iter().cloned());
            break;
        }
        if arg == "-n" || arg == "-D" {
            i += 1;
            continue;
        }
        if arg == "--machine" {
            config.machine = true;
            i += 1;
            continue;
        }
        if arg[1..].chars().all(|flag| matches!(flag, 'n' | 'D')) {
            i += 1;
            continue;
        }
        if arg == "-a" || arg == "-A" || arg == "-u" || arg == "-m" {
            let value = args
                .get(i + 1)
                .ok_or_else(|| cli_error(format!("missing value for {arg}")))?
                .clone();
            set_global_option(&mut config, arg, value)?;
            i += 2;
            continue;
        }
        if let Some(value) = arg.strip_prefix("-a") {
            set_global_option(&mut config, "-a", value.to_string())?;
            i += 1;
            continue;
        }
        if let Some(value) = arg.strip_prefix("-A") {
            set_global_option(&mut config, "-A", value.to_string())?;
            i += 1;
            continue;
        }
        if let Some(value) = arg.strip_prefix("-u") {
            set_global_option(&mut config, "-u", value.to_string())?;
            i += 1;
            continue;
        }
        if let Some(value) = arg.strip_prefix("-m") {
            set_global_option(&mut config, "-m", value.to_string())?;
            i += 1;
            continue;
        }
        return Err(cli_error(format!("unknown option {arg}")));
    }
    *args = rest;
    Ok(config)
}

fn set_global_option(config: &mut Config, option: &str, value: String) -> CliResult<()> {
    match option {
        "-a" => config.address = Some(value),
        "-A" => config.aname = value,
        "-u" => config.uname = value,
        "-m" => {
            config.msize = value
                .parse()
                .map_err(|_| cli_error(format!("invalid msize {value}")))?;
        }
        _ => return Err(cli_error(format!("unknown option {option}"))),
    }
    Ok(())
}

pub(crate) fn usage() -> ! {
    eprintln!(
        "usage: r9p [-n] [--machine] [-a address] [-A aname] [-u uname] [-m msize] cmd args..."
    );
    eprintln!("possible cmds:");
    eprintln!("  version [service]");
    eprintln!("  attach [service]");
    eprintln!("  read name");
    eprintln!("  readfd name");
    eprintln!("  write [-l] name");
    eprintln!("  write-at name offset");
    eprintln!("  writefd name");
    eprintln!("  stat name");
    eprintln!("  rdwr name");
    eprintln!("  ls [-ldnt] name...");
    eprintln!("  list name                machine mode");
    eprintln!("  read-to name local       machine mode");
    eprintln!("  rm name...");
    eprintln!("  remove name              machine mode");
    eprintln!("  write-from name offset local  machine mode");
    eprintln!("  create name...");
    eprintln!("  mkdir name...");
    eprintln!("  con [-r] name");
    eprintln!("without -a, name elem/path means /path on server unix!$NAMESPACE/elem");
    std::process::exit(2);
}

#[cfg(test)]
mod tests {
    use super::parse_global_options;
    use crate::commands::mutate::split_parent;
    use crate::format::{
        format_attach, format_stat, format_version, hex_decode, hex_encode, mode_string,
    };
    use crate::io::parse_offset;
    use crate::target::split_namespace_path;
    use r9p::qid::DMDIR;
    use r9p::{qid::Qid, stat::Stat};

    #[test]
    fn global_options_accept_plan9port_flags_and_extensions() {
        let mut args = vec![
            "-nD".to_string(),
            "--machine".to_string(),
            "-a".to_string(),
            "tcp!127.0.0.1!9564".to_string(),
            "-A/".to_string(),
            "-ucodex".to_string(),
            "-m65536".to_string(),
            "ls".to_string(),
            "/".to_string(),
        ];
        let config = parse_global_options(&mut args).expect("options should parse");
        assert_eq!(config.address.as_deref(), Some("tcp!127.0.0.1!9564"));
        assert_eq!(config.aname, "/");
        assert_eq!(config.uname, "codex");
        assert_eq!(config.msize, 65_536);
        assert!(config.machine);
        assert_eq!(args, ["ls".to_string(), "/".to_string()]);
    }

    #[test]
    fn namespace_paths_split_like_plan9port_service_paths() {
        let (service, path) =
            split_namespace_path("acme/123/body").expect("namespace path should split");
        assert_eq!(service, "acme");
        assert_eq!(path, "123/body");
    }

    #[test]
    fn create_paths_split_parent_and_leaf() {
        let (parent, name) = split_parent("/entries/new.md").expect("path should split");
        assert_eq!(parent, "/entries");
        assert_eq!(name, "new.md");
    }

    #[test]
    fn write_at_offset_parses_as_decimal_count() {
        assert_eq!(parse_offset("42").expect("offset should parse"), 42);
        assert!(parse_offset("four").is_err());
    }

    #[test]
    fn ls_mode_and_stat_formats_follow_plan9port_shape() {
        let stat = Stat::new("entries", Qid::dir(7), DMDIR | 0o755);
        assert_eq!(mode_string(stat.mode), "d-rwxr-xr-x");
        assert!(format_stat(&stat).contains("q (0000000000000007 0 d)"));
    }

    #[test]
    fn version_and_attach_formats_match_vault_operator_shape() {
        assert_eq!(
            format_version(65_536, b"9P2000"),
            "version=9P2000 msize=65536"
        );
        assert_eq!(format_attach(Qid::dir(42)), "attached qid=dir/0/42");
    }

    #[test]
    fn machine_payloads_are_hex_encoded() {
        assert_eq!(hex_encode(b"9P2000"), "395032303030");
        assert_eq!(
            hex_decode("7661756c74").expect("hex should decode"),
            b"vault"
        );
        assert!(hex_decode("abc").is_err());
    }
}
