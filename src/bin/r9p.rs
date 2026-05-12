use r9p::{
    blocking::{self, BoxedClient, ReadWrite, ORDWR, OREAD, OTRUNC, OWRITE},
    fid::Fid,
    qid::{DMAPPEND, DMDIR},
    stat::{decode_dir_entries, Stat},
};
use std::{
    env,
    error::Error,
    io::{self, BufRead, Read, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::net::UnixStream;

type CliResult<T> = Result<T, Box<dyn Error>>;

const DEFAULT_MSIZE: u32 = 65_536;
const READ_CHUNK: u32 = 65_536;
const CTRL_R: u8 = b'R' - b'A' + 1;

const DMEXCL: u32 = 0x2000_0000;
const DMAUTH: u32 = 0x0800_0000;
const DMSYMLINK: u32 = 0x0200_0000;
const DMDEVICE: u32 = 0x0080_0000;
const DMNAMEDPIPE: u32 = 0x0020_0000;
const DMSOCKET: u32 = 0x0010_0000;

#[derive(Clone, Debug)]
struct Config {
    address: Option<String>,
    aname: String,
    uname: String,
    msize: u32,
}

#[derive(Clone, Debug)]
struct Target {
    config: Config,
    path: String,
}

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
        "read" | "readfd" => read_cmd(config, args),
        "write" => write_cmd(config, args, true),
        "writefd" => write_cmd(config, args, false),
        "stat" => stat_cmd(config, args),
        "rdwr" => rdwr_cmd(config, args),
        "ls" => ls_cmd(config, args),
        "rm" => rm_cmd(config, args),
        "create" => create_cmd(config, args),
        "con" => con_cmd(config, args),
        _ => {
            usage();
        }
    }
}

fn parse_global_options(args: &mut Vec<String>) -> CliResult<Config> {
    let mut config = Config {
        address: None,
        aname: String::new(),
        uname: env::var("USER").unwrap_or_else(|_| "none".to_string()),
        msize: DEFAULT_MSIZE,
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

fn read_cmd(config: Config, args: Vec<String>) -> CliResult<()> {
    if args.len() != 1 {
        usage();
    }
    let target = Target {
        config,
        path: args[0].clone(),
    };
    let (mut client, fid) = open_path(&target, OREAD)?;
    let result = copy_fid_to_stdout(&mut client, fid);
    let clunk = client.clunk(fid);
    result?;
    clunk?;
    Ok(())
}

fn write_cmd(config: Config, mut args: Vec<String>, allow_line_option: bool) -> CliResult<()> {
    let by_line = if allow_line_option && args.first().is_some_and(|arg| arg == "-l") {
        args.remove(0);
        true
    } else {
        false
    };
    if args.len() != 1 {
        usage();
    }
    let target = Target {
        config,
        path: args[0].clone(),
    };
    let (mut client, fid) = open_path(&target, OWRITE | OTRUNC)?;
    let result = copy_stdin_to_fid(&mut client, fid, by_line);
    let clunk = client.clunk(fid);
    result?;
    clunk?;
    Ok(())
}

fn stat_cmd(config: Config, args: Vec<String>) -> CliResult<()> {
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
    println!("{}", format_stat(&stat));
    client.clunk(fid)?;
    Ok(())
}

fn rdwr_cmd(config: Config, args: Vec<String>) -> CliResult<()> {
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

fn ls_cmd(config: Config, mut args: Vec<String>) -> CliResult<()> {
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

fn rm_cmd(config: Config, args: Vec<String>) -> CliResult<()> {
    if args.is_empty() {
        usage();
    }
    let mut had_error = false;
    for path in args {
        let target = Target {
            config: config.clone(),
            path: path.clone(),
        };
        match remove_one(&target) {
            Ok(()) => {}
            Err(error) => {
                eprintln!("remove {path}: {error}");
                had_error = true;
            }
        }
    }
    if had_error {
        return Err(cli_error("remove errors"));
    }
    Ok(())
}

fn create_cmd(config: Config, args: Vec<String>) -> CliResult<()> {
    if args.is_empty() {
        usage();
    }
    let mut had_error = false;
    for path in args {
        let target = Target {
            config: config.clone(),
            path: path.clone(),
        };
        match create_one(&target) {
            Ok(()) => {}
            Err(error) => {
                eprintln!("create {path}: {error}");
                had_error = true;
            }
        }
    }
    if had_error {
        return Err(cli_error("create errors"));
    }
    Ok(())
}

fn con_cmd(config: Config, mut args: Vec<String>) -> CliResult<()> {
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

fn con_writer(target: Target) -> CliResult<()> {
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

fn open_path(target: &Target, mode: u8) -> CliResult<(BoxedClient, Fid)> {
    let (mut client, path) = connect_path(target)?;
    let fid = client.walk_path(&path)?;
    client.open(fid, mode)?;
    Ok((client, fid))
}

fn connect_path(target: &Target) -> CliResult<(BoxedClient, String)> {
    match &target.config.address {
        Some(address) => {
            let stream = dial_address(address)?;
            let client = blocking::Client::connect(
                stream,
                &target.config.uname,
                &target.config.aname,
                target.config.msize,
            )?;
            Ok((client, target.path.clone()))
        }
        None => {
            let (service, path) = split_namespace_path(&target.path)?;
            let socket = namespace_socket(&service)?;
            let stream = dial_unix_socket(&socket)?;
            let client = blocking::Client::connect(
                stream,
                &target.config.uname,
                &target.config.aname,
                target.config.msize,
            )?;
            Ok((client, path))
        }
    }
}

fn dial_address(address: &str) -> CliResult<Box<dyn ReadWrite>> {
    if let Some(path) = address.strip_prefix("unix!") {
        return dial_unix_socket(Path::new(path));
    }
    let socket = blocking::parse_tcp_address(address)?;
    let stream = TcpStream::connect(&socket)
        .map_err(|error| cli_error(format!("connect {socket}: {error}")))?;
    stream
        .set_nodelay(true)
        .map_err(|error| cli_error(format!("set TCP_NODELAY: {error}")))?;
    Ok(Box::new(stream))
}

#[cfg(unix)]
fn dial_unix_socket(path: &Path) -> CliResult<Box<dyn ReadWrite>> {
    let stream = UnixStream::connect(path)
        .map_err(|error| cli_error(format!("connect {}: {error}", path.display())))?;
    Ok(Box::new(stream))
}

#[cfg(not(unix))]
fn dial_unix_socket(path: &Path) -> CliResult<Box<dyn ReadWrite>> {
    Err(cli_error(format!(
        "unix sockets are not supported on this platform: {}",
        path.display()
    )))
}

fn split_namespace_path(path: &str) -> CliResult<(String, String)> {
    let trimmed = path.trim_start_matches('/');
    let (service, rest) = match trimmed.split_once('/') {
        Some((service, rest)) => (service, rest),
        None => (trimmed, ""),
    };
    if service.is_empty() {
        return Err(cli_error(
            "without -a, path must be service/path for a namespace socket",
        ));
    }
    Ok((service.to_string(), rest.to_string()))
}

fn namespace_socket(service: &str) -> CliResult<PathBuf> {
    let namespace = env::var("NAMESPACE")
        .map_err(|_| cli_error("NAMESPACE is required when -a is not provided"))?;
    Ok(PathBuf::from(namespace).join(service))
}

fn copy_fid_to_stdout(client: &mut BoxedClient, fid: Fid) -> CliResult<()> {
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

fn copy_stdin_to_fid(client: &mut BoxedClient, fid: Fid, by_line: bool) -> CliResult<()> {
    let mut stdin = io::BufReader::new(io::stdin().lock());
    let mut offset = 0_u64;
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

fn write_exact_count(
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

#[derive(Debug)]
struct LsOptions {
    long: bool,
    directory: bool,
    no_sort: bool,
    sort_time: bool,
}

fn parse_ls_options(args: &mut Vec<String>) -> CliResult<LsOptions> {
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

fn ls_one(target: &Target, options: &LsOptions) -> CliResult<()> {
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

fn read_dir_stats(client: &mut BoxedClient, fid: Fid) -> CliResult<Vec<Stat>> {
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

fn print_ls_entries(stats: &[Stat], long: bool) {
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
struct LsWidths {
    dev: usize,
    uid: usize,
    gid: usize,
    len: usize,
}

impl LsWidths {
    fn from_stats(stats: &[Stat]) -> Self {
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

fn remove_one(target: &Target) -> CliResult<()> {
    let (mut client, path) = connect_path(target)?;
    let fid = client.walk_path(&path)?;
    client.remove(fid)?;
    Ok(())
}

fn create_one(target: &Target) -> CliResult<()> {
    if target.config.address.is_none() && !target.path.trim_start_matches('/').contains('/') {
        return Err(cli_error("without -a, create path must be service/name"));
    }
    let (parent, name) = split_parent(&target.path)?;
    let parent_target = Target {
        config: target.config.clone(),
        path: parent,
    };
    let (mut client, path) = connect_path(&parent_target)?;
    let parent_fid = client.walk_path(&path)?;
    let (fid, _) = client.create(parent_fid, name.as_bytes(), 0o666, OREAD)?;
    client.clunk(fid)?;
    client.clunk(parent_fid)?;
    Ok(())
}

fn split_parent(path: &str) -> CliResult<(String, String)> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(cli_error("cannot create root"));
    }
    let (parent, name) = match trimmed.rsplit_once('/') {
        Some(("", name)) => ("/".to_string(), name.to_string()),
        Some((parent, name)) => (parent.to_string(), name.to_string()),
        None => (".".to_string(), trimmed.to_string()),
    };
    if name.is_empty() || name == "." || name == ".." {
        return Err(cli_error(format!("bad create name {name}")));
    }
    Ok((parent, name))
}

fn format_stat(stat: &Stat) -> String {
    format!(
        "'{}' '{}' '{}' '{}' q ({:016x} {} {}) m 0{:o} at {} mt {} l {} t {} d {}",
        text(&stat.name),
        text(&stat.uid),
        text(&stat.gid),
        text(&stat.muid),
        stat.qid.path,
        stat.qid.version,
        qid_type_string(stat.qid.qtype),
        stat.mode,
        stat.atime,
        stat.mtime,
        stat.length,
        stat.type_,
        stat.dev
    )
}

fn qid_type_string(qtype: u8) -> String {
    let mut out = String::new();
    if qtype & 0x80 != 0 {
        out.push('d');
    }
    if qtype & 0x40 != 0 {
        out.push('a');
    }
    if qtype & 0x20 != 0 {
        out.push('l');
    }
    if qtype & 0x08 != 0 {
        out.push('A');
    }
    out
}

fn mode_string(mode: u32) -> String {
    let mut out = String::with_capacity(11);
    out.push(if mode & DMDIR != 0 {
        'd'
    } else if mode & DMAPPEND != 0 {
        'a'
    } else if mode & DMAUTH != 0 {
        'A'
    } else if mode & DMDEVICE != 0 {
        'D'
    } else if mode & DMSOCKET != 0 {
        'S'
    } else if mode & DMNAMEDPIPE != 0 {
        'P'
    } else {
        '-'
    });
    out.push(if mode & DMEXCL != 0 {
        'l'
    } else if mode & DMSYMLINK != 0 {
        'L'
    } else {
        '-'
    });
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 7;
        out.push(if bits & 4 != 0 { 'r' } else { '-' });
        out.push(if bits & 2 != 0 { 'w' } else { '-' });
        out.push(if bits & 1 != 0 { 'x' } else { '-' });
    }
    out
}

fn time_string(mtime: u32) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let mtime_u64 = u64::from(mtime);
    let components = utc_components(mtime_u64);
    if now.saturating_sub(mtime_u64) < 6 * 30 * 24 * 60 * 60 {
        format!(
            "{} {:>2} {:02}:{:02}",
            month_name(components.month),
            components.day,
            components.hour,
            components.minute
        )
    } else {
        format!(
            "{} {:>2} {:>5}",
            month_name(components.month),
            components.day,
            components.year
        )
    }
}

#[derive(Debug)]
struct DateTime {
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
}

fn utc_components(seconds: u64) -> DateTime {
    let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX);
    let secs_of_day = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    DateTime {
        year,
        month,
        day,
        hour: u32::try_from(secs_of_day / 3600).unwrap_or(0),
        minute: u32::try_from((secs_of_day % 3600) / 60).unwrap_or(0),
    }
}

fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if month <= 2 { 1 } else { 0 };
    (
        i32::try_from(year).unwrap_or(i32::MAX),
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
    )
}

fn month_name(month: u32) -> &'static str {
    match month {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        12 => "Dec",
        _ => "???",
    }
}

fn quote_name(bytes: &[u8]) -> String {
    let value = text(bytes);
    if !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | '+'))
    {
        return value;
    }
    let mut out = String::from("'");
    for ch in value.chars() {
        if ch == '\'' {
            out.push('\'');
        }
        out.push(ch);
    }
    out.push('\'');
    out
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn cli_error(message: impl Into<String>) -> Box<dyn Error> {
    Box::new(io::Error::other(message.into()))
}

fn usage() -> ! {
    eprintln!("usage: r9p [-n] [-a address] [-A aname] [-u uname] [-m msize] cmd args...");
    eprintln!("possible cmds:");
    eprintln!("  read name");
    eprintln!("  readfd name");
    eprintln!("  write [-l] name");
    eprintln!("  writefd name");
    eprintln!("  stat name");
    eprintln!("  rdwr name");
    eprintln!("  ls [-ldnt] name...");
    eprintln!("  rm name...");
    eprintln!("  create name...");
    eprintln!("  con [-r] name");
    eprintln!("without -a, name elem/path means /path on server unix!$NAMESPACE/elem");
    std::process::exit(2);
}

#[cfg(test)]
mod tests {
    use super::{
        format_stat, mode_string, parse_global_options, split_namespace_path, split_parent, DMDIR,
    };
    use r9p::{qid::Qid, stat::Stat};

    #[test]
    fn global_options_accept_plan9port_flags_and_extensions() {
        let mut args = vec![
            "-nD".to_string(),
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
    fn ls_mode_and_stat_formats_follow_plan9port_shape() {
        let stat = Stat::new("entries", Qid::dir(7), DMDIR | 0o755);
        assert_eq!(mode_string(stat.mode), "d-rwxr-xr-x");
        assert!(format_stat(&stat).contains("q (0000000000000007 0 d)"));
    }
}
