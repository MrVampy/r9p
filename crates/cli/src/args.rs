use std::{
    env,
    io::{self, Write},
    path::PathBuf,
    time::Duration,
};

use clap::{ArgAction, ArgGroup, Args as ClapArgs, CommandFactory, Parser, Subcommand};

use crate::{
    commands::{
        auth_keygen::auth_keygen_cmd,
        cert::cert_cmd,
        con::con_cmd,
        ls::ls_cmd,
        machine::{machine_list_cmd, machine_remove_cmd, machine_rpc_hex_cmd},
        mount::mount_cmd,
        mutate::{create_cmd, mkdir_cmd, rm_cmd},
        read_write::{
            create_write_from_cmd, read_cmd, read_to_cmd, rpc_cmd, write_at_cmd, write_cmd,
            write_from_cmd, write_from_trunc_cmd, ReadMode, WriteMode,
        },
        reverse::{reverse_broker_cmd, reverse_export_cmd, session_proxy_cmd},
        script::machine_script_cmd,
        serve::{export_cmd, serve_cmd},
        session::session_cmd,
        stat_rdwr::{rdwr_cmd, stat_cmd},
        stream::stream_cmd,
        stream_export::stream_export_cmd,
        version_attach::{attach_cmd, version_cmd},
    },
    errors::CliResult,
    target::Config,
    DEFAULT_MSIZE,
};

#[derive(Debug, Parser)]
#[command(
    name = "r9p",
    version,
    about = "Operate on 9P services and composed namespaces",
    long_about = "Operate on 9P services and composed namespaces.\n\nWithout --bind, a path such as memory/status resolves through the Unix socket at $NAMESPACE/memory. With --bind, paths are relative to that explicit endpoint.",
    arg_required_else_help = true,
    subcommand_required = true
)]
struct Cli {
    /// Retained for plan9port command-line compatibility.
    #[arg(short = 'n', action = ArgAction::SetTrue, hide = true)]
    _no_cache: bool,
    /// Retained as a global plan9port compatibility flag.
    #[arg(short = 'D', action = ArgAction::SetTrue, hide = true)]
    _debug: bool,
    /// Emit the stable machine-oriented command formats.
    #[arg(long)]
    machine: bool,
    /// Dial an explicit endpoint instead of resolving the first path element in $NAMESPACE.
    #[arg(short = 'a', long = "bind", value_name = "ADDRESS")]
    address: Option<String>,
    /// Attach name sent to the root service.
    #[arg(short = 'A', value_name = "ANAME")]
    aname: Option<String>,
    /// 9P user name. Defaults to $USER or "none".
    #[arg(short = 'u', value_name = "UNAME")]
    uname: Option<String>,
    /// Requested 9P message size.
    #[arg(short = 'm', value_name = "BYTES")]
    msize: Option<u32>,
    /// Client session-auth configuration.
    #[arg(long, value_name = "PATH")]
    auth_config: Option<PathBuf>,
    /// Certified responder expected for the root authenticated session.
    #[arg(long, value_name = "NAME")]
    auth_domain: Option<String>,
    /// Ordinary request timeout in seconds. Zero disables it.
    #[arg(
        long,
        default_value = "30",
        value_name = "SECONDS",
        allow_hyphen_values = true,
        value_parser = parse_nonnegative_seconds
    )]
    request_timeout: f64,
    /// Control-operation timeout in seconds. Zero disables it.
    #[arg(
        long,
        default_value = "600",
        value_name = "SECONDS",
        allow_hyphen_values = true,
        value_parser = parse_nonnegative_seconds
    )]
    control_timeout: f64,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Generate an Ed25519 certificate key pair.
    AuthKeygen(AuthKeygenArgs),
    /// Create, sign, inspect, or verify r9p certificates.
    Cert(CertArgs),
    /// Negotiate and print the remote 9P version.
    Version(ConnectionArgs),
    /// Attach and print the root qid.
    Attach(ConnectionArgs),
    /// Read a file to stdout.
    #[command(visible_alias = "cat")]
    Read(PathArg),
    /// Stream a file descriptor to stdout without machine encoding.
    Readfd(PathArg),
    /// Read a remote file into a local file in machine mode.
    ReadTo(ReadToArgs),
    /// Write stdin to a file, or write machine-mode hex at an offset.
    Write(WriteArgs),
    /// Write stdin at an explicit byte offset.
    WriteAt(WriteAtArgs),
    /// Stream stdin into a truncated file without machine decoding.
    Writefd(PathArg),
    /// Write a local file at an offset in machine mode.
    WriteFrom(WriteFromArgs),
    /// Replace a remote file from a local file in machine mode.
    WriteFromTrunc(WriteFromTruncArgs),
    /// Create a remote file and fill it from a local file in machine mode.
    CreateWriteFrom(CreateWriteFromArgs),
    /// Run a tab-delimited machine-mode operation script.
    Script(ScriptArgs),
    /// Perform a same-fid RPC using hexadecimal payloads in machine mode.
    RpcHex(RpcHexArgs),
    /// Host or inspect a reusable client session.
    Session(SessionArgs),
    /// Mount a 9P namespace through FUSE or supervise a declared mount.
    Mount(Box<MountArgs>),
    /// Serve a local filesystem over a local 9P endpoint.
    Serve(ServeArgs),
    /// Export a local filesystem with an optional authenticated boundary.
    Export(ExportArgs),
    /// Export one fixed process as an authenticated full-duplex 9P stream.
    StreamExport(StreamExportArgs),
    /// Accept reverse exporters and publish a local or authenticated proxy.
    ReverseBroker(ReverseBrokerArgs),
    /// Export a filesystem through a reverse broker.
    ReverseExport(ReverseExportArgs),
    /// Publish a local proxy for authenticated upstream sessions.
    SessionProxy(SessionProxyArgs),
    /// Print file metadata.
    Stat(PathArg),
    /// Run an interactive read/write exchange on one fid.
    Rdwr(PathArg),
    /// Write one request and read its response on the same fid.
    Rpc(RpcArgs),
    /// List directory entries or file metadata.
    Ls(LsArgs),
    /// List stable machine records in machine mode.
    List(PathArg),
    /// Remove one or more paths.
    Rm(PathsArgs),
    /// Remove one path with stable machine output in machine mode.
    Remove(PathArg),
    /// Create one or more files, or one explicitly shaped machine-mode file.
    Create(CreateArgs),
    /// Create one or more directories.
    Mkdir(PathsArgs),
    /// Connect stdin and stdout to a full-duplex file.
    Con(ConArgs),
    /// Carry an unchanged full-duplex stdio byte stream over one 9P session.
    Stream(PathArg),
}

#[derive(Debug, ClapArgs)]
struct AuthKeygenArgs {
    /// Destination for the private key.
    #[arg(long, value_name = "PATH")]
    private: String,
    /// Destination for the public key.
    #[arg(long, value_name = "PATH")]
    public: String,
    /// Filesystem access policy for the private key.
    #[arg(
        long,
        value_name = "POLICY",
        default_value = "owner-only",
        value_parser = ["owner-only", "owner-group-read"]
    )]
    private_access: String,
}

#[derive(Debug, ClapArgs)]
struct CertArgs {
    #[command(subcommand)]
    command: CertCommand,
}

#[derive(Debug, Subcommand)]
enum CertCommand {
    /// Generate an offline root signing key pair.
    Root(CertRootArgs),
    /// Sign one service or client certificate.
    Sign(CertSignArgs),
    /// Print bounded certificate facts.
    Print(CertPrintArgs),
    /// Verify a certificate against an expected root.
    Verify(CertVerifyArgs),
}

#[derive(Debug, ClapArgs)]
struct CertRootArgs {
    #[arg(long, value_name = "PATH")]
    private: String,
    #[arg(long, value_name = "PATH")]
    public: String,
}

#[derive(Debug, ClapArgs)]
#[command(group(
    ArgGroup::new("subject_key")
        .required(true)
        .multiple(false)
        .args(["key", "key_file"])
))]
#[command(group(
    ArgGroup::new("validity_end")
        .required(true)
        .multiple(false)
        .args(["days", "not_after"])
))]
struct CertSignArgs {
    #[arg(long, value_name = "PATH")]
    root_private: String,
    #[arg(long, value_name = "NAME")]
    name: String,
    #[arg(long, value_name = "HEX")]
    key: Option<String>,
    #[arg(long, value_name = "PATH")]
    key_file: Option<String>,
    #[arg(long, value_name = "GROUP", action = ArgAction::Append)]
    group: Vec<String>,
    #[arg(long, value_name = "DAYS")]
    days: Option<u64>,
    #[arg(long, value_name = "UNIX_SECONDS")]
    not_before: Option<u64>,
    #[arg(long, value_name = "UNIX_SECONDS")]
    not_after: Option<u64>,
    #[arg(long, value_name = "PATH")]
    out: Option<String>,
}

#[derive(Debug, ClapArgs)]
struct CertPrintArgs {
    #[arg(long, value_name = "PATH")]
    path: String,
    #[arg(long, value_name = "UNIX_SECONDS")]
    at: Option<u64>,
}

#[derive(Debug, ClapArgs)]
#[command(group(
    ArgGroup::new("root_key")
        .required(true)
        .multiple(false)
        .args(["root", "root_file"])
))]
struct CertVerifyArgs {
    #[arg(long, value_name = "PATH")]
    path: String,
    #[arg(long, value_name = "HEX")]
    root: Option<String>,
    #[arg(long, value_name = "PATH")]
    root_file: Option<String>,
    #[arg(long, value_name = "UNIX_SECONDS")]
    at: Option<u64>,
}

#[derive(Debug, ClapArgs)]
struct ConnectionArgs {
    /// Namespace service path when --bind is omitted.
    #[arg(value_name = "SERVICE")]
    service: Option<String>,
}

#[derive(Debug, ClapArgs)]
struct PathArg {
    #[arg(value_name = "PATH")]
    path: String,
}

#[derive(Debug, ClapArgs)]
struct PathsArgs {
    #[arg(value_name = "PATH", required = true, num_args = 1..)]
    paths: Vec<String>,
}

#[derive(Debug, ClapArgs)]
struct ReadToArgs {
    #[arg(value_name = "PATH")]
    path: String,
    #[arg(value_name = "LOCAL_FILE")]
    local_file: String,
}

#[derive(Debug, ClapArgs)]
struct WriteArgs {
    /// Write stdin one line per 9P write in interactive text mode.
    #[arg(short = 'l')]
    line: bool,
    #[arg(value_name = "PATH")]
    path: String,
    /// Machine mode only: starting byte offset.
    #[arg(value_name = "OFFSET", requires = "data_hex")]
    offset: Option<String>,
    /// Machine mode only: hexadecimal payload.
    #[arg(value_name = "DATA_HEX", requires = "offset")]
    data_hex: Option<String>,
}

#[derive(Debug, ClapArgs)]
struct WriteAtArgs {
    #[arg(value_name = "PATH")]
    path: String,
    #[arg(value_name = "OFFSET")]
    offset: String,
}

#[derive(Debug, ClapArgs)]
struct WriteFromArgs {
    #[arg(value_name = "PATH")]
    path: String,
    #[arg(value_name = "OFFSET")]
    offset: String,
    #[arg(value_name = "LOCAL_FILE")]
    local_file: String,
}

#[derive(Debug, ClapArgs)]
struct WriteFromTruncArgs {
    #[arg(value_name = "PATH")]
    path: String,
    #[arg(value_name = "LOCAL_FILE")]
    local_file: String,
}

#[derive(Debug, ClapArgs)]
struct CreateWriteFromArgs {
    #[arg(value_name = "PATH")]
    path: String,
    #[arg(value_name = "PERM")]
    perm: String,
    #[arg(value_name = "MODE")]
    mode: String,
    #[arg(value_name = "OFFSET")]
    offset: String,
    #[arg(value_name = "LOCAL_FILE")]
    local_file: String,
}

#[derive(Debug, ClapArgs)]
struct ScriptArgs {
    /// Service name when --bind is omitted, otherwise the script file.
    #[arg(value_name = "SERVICE_OR_FILE")]
    first: String,
    /// Script file when a namespace service was supplied first.
    #[arg(value_name = "FILE")]
    second: Option<String>,
}

#[derive(Debug, ClapArgs)]
struct RpcHexArgs {
    #[arg(value_name = "PATH")]
    path: String,
    #[arg(value_name = "REQUEST_HEX")]
    request_hex: String,
}

#[derive(Debug, ClapArgs)]
struct RpcArgs {
    #[arg(value_name = "PATH")]
    path: String,
    /// Request text. If omitted, a terminal sends an empty request and a pipe supplies stdin.
    #[arg(value_name = "REQUEST", allow_hyphen_values = true)]
    request: Option<String>,
}

#[derive(Debug, ClapArgs)]
struct LsArgs {
    /// Print long metadata records.
    #[arg(short = 'l')]
    long: bool,
    /// List a directory itself rather than its children.
    #[arg(short = 'd')]
    directory: bool,
    /// Preserve server order.
    #[arg(short = 'n')]
    no_sort: bool,
    /// Sort by modification time instead of name.
    #[arg(short = 't')]
    sort_time: bool,
    #[arg(value_name = "PATH", num_args = 0..)]
    paths: Vec<String>,
}

#[derive(Debug, ClapArgs)]
struct CreateArgs {
    /// Normal mode accepts one or more paths. Machine mode accepts PATH PERM MODE.
    #[arg(value_name = "ARG", required = true, num_args = 1..)]
    values: Vec<String>,
}

#[derive(Debug, ClapArgs)]
struct ConArgs {
    /// Reattach a replay-safe positional stream after definitive transport loss.
    #[arg(long)]
    resume: bool,
    /// Preserve carriage returns instead of stripping them.
    #[arg(short = 'r')]
    preserve_cr: bool,
    #[arg(value_name = "PATH")]
    path: String,
}

#[derive(Debug, ClapArgs)]
struct SessionArgs {
    #[command(subcommand)]
    command: SessionCommand,
}

#[derive(Debug, Subcommand)]
enum SessionCommand {
    /// Host one renewable namespace client behind a local control socket.
    Serve(Box<SessionServeArgs>),
    /// Read the current client-session status.
    Status(SessionSocketArgs),
    /// Materialize a bounded namespace snapshot through the hosted session.
    Snapshot(SessionSnapshotArgs),
    /// Stat one namespace path through the hosted session.
    Stat(SessionOptionalPathArgs),
    /// List one namespace path through the hosted session.
    List(SessionOptionalPathArgs),
    /// Read one namespace path through the hosted session.
    Read(SessionRequiredPathArgs),
}

#[derive(Debug, ClapArgs)]
struct SessionServeArgs {
    /// Local control socket to publish.
    #[arg(long, value_name = "PATH")]
    socket: String,
    /// Namespace change-feed catch-up path.
    #[arg(long, value_name = "NAMESPACE_PATH")]
    change_feed: Option<String>,
    /// Namespace change-feed blocking stream path.
    #[arg(long, value_name = "NAMESPACE_PATH")]
    change_feed_stream: Option<String>,
    /// Catch-up path template containing {event_id}.
    #[arg(long, value_name = "PATH_TEMPLATE")]
    change_feed_cursor_template: Option<String>,
    /// Delay between definitive change-feed reconnect attempts.
    #[arg(long, value_name = "SECONDS")]
    change_feed_reconnect_delay: Option<String>,
    /// Maximum unprocessed change-feed records.
    #[arg(long, value_name = "COUNT")]
    change_feed_backpressure: Option<usize>,
    /// Optional coherent FUSE mountpoint hosted by this session.
    #[arg(long, value_name = "PATH")]
    mount: Option<String>,
    /// Namespace subtree exposed by the optional mount.
    #[arg(long, value_name = "NAMESPACE_PATH")]
    mount_source: Option<String>,
    /// Optional mount status output file.
    #[arg(long, value_name = "PATH")]
    mount_status_file: Option<String>,
    /// Optional mount diagnostics output file.
    #[arg(long, value_name = "PATH")]
    mount_diagnostics_file: Option<String>,
    #[arg(long, value_name = "SECONDS")]
    mount_attr_timeout: Option<String>,
    #[arg(long, value_name = "SECONDS")]
    mount_entry_timeout: Option<String>,
    #[arg(long, value_name = "SECONDS")]
    mount_negative_timeout: Option<String>,
    /// Permit coherent kernel read caching when a feed proves freshness.
    #[arg(long)]
    mount_coherent_read_cache: bool,
    /// Endpoint to host. May be omitted when global --bind supplies it.
    #[arg(value_name = "ENDPOINT")]
    endpoint: Option<String>,
}

#[derive(Debug, ClapArgs)]
struct SessionSocketArgs {
    #[arg(long, value_name = "PATH")]
    socket: String,
}

#[derive(Debug, ClapArgs)]
struct SessionSnapshotArgs {
    #[arg(long, value_name = "PATH")]
    socket: String,
    #[arg(long, value_name = "DEPTH")]
    depth: Option<usize>,
    #[arg(value_name = "NAMESPACE_PATH")]
    path: Option<String>,
}

#[derive(Debug, ClapArgs)]
struct SessionOptionalPathArgs {
    #[arg(long, value_name = "PATH")]
    socket: String,
    #[arg(value_name = "NAMESPACE_PATH")]
    path: Option<String>,
}

#[derive(Debug, ClapArgs)]
struct SessionRequiredPathArgs {
    #[arg(long, value_name = "PATH")]
    socket: String,
    #[arg(value_name = "NAMESPACE_PATH")]
    path: String,
}

#[derive(Debug, ClapArgs)]
struct MountArgs {
    /// Direct endpoint, or one of ensure, read-ahead, status, and stop.
    #[arg(value_name = "ENDPOINT_OR_ACTION")]
    endpoint_or_action: String,
    /// Direct-mode mountpoint.
    #[arg(value_name = "MOUNTPOINT")]
    direct_mountpoint: Option<String>,

    /// Ordered endpoints tried after the primary endpoint is unavailable.
    #[arg(long, value_name = "ENDPOINT")]
    fallback_endpoint: Vec<String>,

    /// Namespace subtree mounted in direct mode.
    #[arg(long, value_name = "NAMESPACE_PATH")]
    source: Option<String>,
    #[arg(short = 'A', long, value_name = "ANAME")]
    aname: Option<String>,
    #[arg(short = 'u', long, value_name = "UNAME")]
    uname: Option<String>,
    #[arg(short = 'm', long, value_name = "BYTES")]
    msize: Option<u32>,
    #[arg(short = 'D', long)]
    debug: bool,
    #[arg(long)]
    allow_other: bool,
    #[arg(long)]
    coherent_read_cache: bool,
    #[arg(long, value_name = "SECONDS")]
    attr_timeout: Option<String>,
    #[arg(long, value_name = "SECONDS")]
    entry_timeout: Option<String>,
    #[arg(long, value_name = "SECONDS")]
    negative_timeout: Option<String>,
    #[arg(long, value_name = "SECONDS")]
    request_timeout: Option<String>,
    #[arg(long, value_name = "SECONDS")]
    connect_timeout: Option<String>,
    #[arg(long, value_name = "SECONDS")]
    lookup_timeout: Option<String>,
    #[arg(long, value_name = "SECONDS")]
    read_timeout: Option<String>,
    #[arg(long, value_name = "SECONDS")]
    change_feed_read_timeout: Option<String>,
    #[arg(long, value_name = "SECONDS")]
    write_timeout: Option<String>,
    #[arg(long, value_name = "SECONDS")]
    mutation_timeout: Option<String>,
    #[arg(long, value_name = "SECONDS")]
    control_timeout: Option<String>,
    #[arg(long, value_name = "SECONDS")]
    interrupt_timeout: Option<String>,
    #[arg(long, value_name = "COUNT")]
    max_workers: Option<usize>,
    #[arg(long, value_name = "COUNT")]
    max_background: Option<u16>,
    #[arg(long, value_name = "COUNT")]
    congestion_threshold: Option<u16>,
    #[arg(long, value_name = "PATH")]
    diagnostics_file: Option<String>,
    #[arg(long, value_name = "COUNT")]
    diagnostics_capacity: Option<usize>,
    #[arg(long, value_name = "PATH")]
    status_file: Option<String>,
    #[arg(long, value_name = "NAMESPACE_PATH")]
    change_feed: Option<String>,
    #[arg(long, value_name = "NAMESPACE_PATH")]
    change_feed_stream: Option<String>,
    #[arg(long, value_name = "PATH_TEMPLATE")]
    change_feed_cursor_template: Option<String>,
    #[arg(long, value_name = "SCOPE")]
    change_feed_scope: Option<String>,
    #[arg(long, value_name = "SECONDS")]
    change_feed_reconnect_delay: Option<String>,
    #[arg(long, value_name = "COUNT")]
    change_feed_backpressure: Option<usize>,

    /// Supervisor-mode mountpoint.
    #[arg(long = "mountpoint", value_name = "PATH")]
    supervised_mountpoint: Option<String>,
    #[arg(long, value_name = "UNIT")]
    unit: Option<String>,
    #[arg(long, value_name = "SCOPE", value_parser = ["user", "system"])]
    unit_scope: Option<String>,
    #[arg(long, value_name = "ENDPOINT")]
    expect_endpoint: Option<String>,
    #[arg(long, value_name = "PATH")]
    expect_status_file: Option<String>,
    #[arg(long, value_name = "NAMESPACE_PATH")]
    expect_change_feed: Option<String>,
    #[arg(long, value_name = "COUNT")]
    attempts: Option<usize>,
    #[arg(long = "kilobytes", value_name = "COUNT")]
    read_ahead_kilobytes: Option<u64>,
    /// Direct mount arguments for `mount ensure`.
    #[arg(last = true, num_args = 0.., value_name = "MOUNT_ARG")]
    mount_args: Vec<String>,
}

#[derive(Debug, ClapArgs)]
struct ServeArgs {
    #[arg(long, value_name = "ADDRESS")]
    bind: Option<String>,
    #[arg(long, value_name = "COUNT")]
    max_fids: Option<usize>,
    #[arg(long)]
    writable: bool,
    #[arg(value_name = "ROOT")]
    root: String,
}

#[derive(Debug, ClapArgs)]
struct ExportArgs {
    #[arg(long, value_name = "ADDRESS")]
    bind: Option<String>,
    #[arg(long, value_name = "COUNT")]
    max_fids: Option<usize>,
    #[arg(long)]
    writable: bool,
    #[arg(long, value_name = "FORMAT", default_value = "machine")]
    descriptor: String,
    #[arg(long, value_name = "PATH")]
    descriptor_file: Option<String>,
    #[arg(long, value_name = "PATH")]
    auth_config: Option<String>,
    #[arg(long, value_name = "KEY=VALUE", action = ArgAction::Append)]
    descriptor_field: Vec<String>,
    #[arg(value_name = "ROOT")]
    root: String,
}

#[derive(Debug, ClapArgs)]
struct StreamExportArgs {
    #[arg(long, value_name = "ADDRESS")]
    bind: String,
    #[arg(long, value_name = "PATH")]
    auth_config: String,
    #[arg(long, value_name = "NAME", action = ArgAction::Append, required = true)]
    allow_principal: Vec<String>,
    #[arg(long, value_name = "COUNT")]
    max_sessions: Option<usize>,
    #[arg(long, value_name = "BYTES")]
    max_buffer_bytes: Option<usize>,
    #[arg(long, value_name = "PATH")]
    status_file: Option<String>,
    #[arg(last = true, num_args = 1.., value_name = "COMMAND")]
    command: Vec<String>,
}

#[derive(Debug, ClapArgs)]
struct ReverseBrokerArgs {
    #[arg(long, value_name = "ADDRESS")]
    reverse_bind: String,
    #[arg(long, value_name = "ADDRESS_OR_UNIX_PATH")]
    proxy_bind: Option<String>,
    #[arg(
        long,
        value_name = "EXPOSURE",
        value_parser = ["local", "authenticated-network"]
    )]
    proxy_exposure: Option<String>,
    #[arg(long, value_name = "PATH")]
    auth_config: Option<String>,
    #[arg(long, value_name = "NAME")]
    principal: String,
    #[arg(long, value_name = "COUNT")]
    pool: Option<usize>,
    #[arg(long, value_name = "SECONDS")]
    auth_timeout: Option<String>,
    #[arg(long, value_name = "SECONDS")]
    proxy_wait_timeout: Option<String>,
}

#[derive(Debug, ClapArgs)]
struct ReverseExportArgs {
    #[arg(long, value_name = "ADDRESS")]
    connect: String,
    #[arg(long, value_name = "PATH")]
    auth_config: Option<String>,
    #[arg(long, value_name = "NAME")]
    principal: String,
    #[arg(long, value_name = "COUNT")]
    pool: Option<usize>,
    #[arg(long, value_name = "COUNT")]
    max_fids: Option<usize>,
    #[arg(long, value_name = "SECONDS")]
    connect_timeout: Option<String>,
    #[arg(long, value_name = "SECONDS")]
    auth_timeout: Option<String>,
    #[arg(long, value_name = "SECONDS")]
    reconnect_min_delay: Option<String>,
    #[arg(long, value_name = "SECONDS")]
    reconnect_max_delay: Option<String>,
    #[arg(long)]
    writable: bool,
    #[arg(value_name = "ROOT")]
    root: String,
}

#[derive(Debug, ClapArgs)]
struct SessionProxyArgs {
    #[arg(long, value_name = "ADDRESS_OR_UNIX_PATH")]
    bind: Option<String>,
    #[arg(long, value_name = "ADDRESS")]
    connect: String,
    #[arg(long, value_name = "PATH")]
    auth_config: Option<String>,
    #[arg(long, value_name = "NAME")]
    principal: String,
    #[arg(long, value_name = "COUNT")]
    max_sessions: Option<usize>,
    #[arg(long, value_name = "SECONDS")]
    connect_timeout: Option<String>,
    #[arg(long, value_name = "SECONDS")]
    auth_timeout: Option<String>,
}

pub(crate) fn run() -> CliResult<()> {
    let (config, command) = Cli::parse().into_parts();
    match command {
        Command::AuthKeygen(args) => auth_keygen_cmd(config, args.into_args()),
        Command::Cert(args) => cert_cmd(config, args.into_args()),
        Command::Version(args) => version_cmd(config, option_vec(args.service)),
        Command::Attach(args) => attach_cmd(config, option_vec(args.service)),
        Command::Read(args) => read_cmd(config, vec![args.path], ReadMode::Read),
        Command::Readfd(args) => read_cmd(config, vec![args.path], ReadMode::ReadFd),
        Command::ReadTo(args) => {
            require_machine(&config);
            read_to_cmd(config, vec![args.path, args.local_file])
        }
        Command::Write(args) => write_cmd(config, args.into_args(), WriteMode::Write),
        Command::WriteAt(args) => write_at_cmd(config, vec![args.path, args.offset]),
        Command::Writefd(args) => write_cmd(config, vec![args.path], WriteMode::WriteFd),
        Command::WriteFrom(args) => {
            require_machine(&config);
            write_from_cmd(config, vec![args.path, args.offset, args.local_file])
        }
        Command::WriteFromTrunc(args) => {
            require_machine(&config);
            write_from_trunc_cmd(config, vec![args.path, args.local_file])
        }
        Command::CreateWriteFrom(args) => {
            require_machine(&config);
            create_write_from_cmd(
                config,
                vec![
                    args.path,
                    args.perm,
                    args.mode,
                    args.offset,
                    args.local_file,
                ],
            )
        }
        Command::Script(args) => {
            require_machine(&config);
            machine_script_cmd(config, args.into_args())
        }
        Command::RpcHex(args) => {
            require_machine(&config);
            machine_rpc_hex_cmd(config, vec![args.path, args.request_hex])
        }
        Command::Session(args) => session_cmd(config, args.into_args()),
        Command::Mount(args) => mount_cmd(config, args.into_args()),
        Command::Serve(args) => serve_cmd(config, args.into_args()),
        Command::Export(args) => export_cmd(config, args.into_args()),
        Command::StreamExport(args) => stream_export_cmd(config, args.into_args()),
        Command::ReverseBroker(args) => reverse_broker_cmd(config, args.into_args()),
        Command::ReverseExport(args) => reverse_export_cmd(config, args.into_args()),
        Command::SessionProxy(args) => session_proxy_cmd(config, args.into_args()),
        Command::Stat(args) => stat_cmd(config, vec![args.path]),
        Command::Rdwr(args) => rdwr_cmd(config, vec![args.path]),
        Command::Rpc(args) => rpc_cmd(config, args.into_args()),
        Command::Ls(args) => ls_cmd(config, args.into_args()),
        Command::List(args) => {
            require_machine(&config);
            machine_list_cmd(config, vec![args.path])
        }
        Command::Rm(args) => rm_cmd(config, args.paths),
        Command::Remove(args) => {
            require_machine(&config);
            machine_remove_cmd(config, vec![args.path])
        }
        Command::Create(args) => create_cmd(config, args.values),
        Command::Mkdir(args) => mkdir_cmd(config, args.paths),
        Command::Con(args) => con_cmd(config, args.into_args()),
        Command::Stream(args) => stream_cmd(config, vec![args.path]),
    }
}

pub(crate) fn usage() -> ! {
    let mut stderr = io::stderr().lock();
    let _ = Cli::command().write_long_help(&mut stderr);
    let _ = writeln!(stderr);
    std::process::exit(2);
}

impl Cli {
    fn into_parts(self) -> (Config, Command) {
        let msize_set = self.msize.is_some();
        let config = Config {
            auth_domain: self.auth_domain,
            address: self.address,
            auth_config: self.auth_config,
            aname: self.aname.unwrap_or_default(),
            uname: self
                .uname
                .unwrap_or_else(|| env::var("USER").unwrap_or_else(|_| "none".to_string())),
            msize: self.msize.unwrap_or(DEFAULT_MSIZE),
            msize_set,
            machine: self.machine,
            request_timeout: timeout(self.request_timeout),
            control_timeout: timeout(self.control_timeout),
        };
        (config, self.command)
    }
}

fn parse_nonnegative_seconds(value: &str) -> Result<f64, String> {
    let seconds = value
        .parse::<f64>()
        .map_err(|_| format!("invalid duration {value}"))?;
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(format!("duration must be finite and non-negative: {value}"));
    }
    Ok(seconds)
}

fn timeout(seconds: f64) -> Option<Duration> {
    if seconds == 0.0 {
        None
    } else {
        Some(Duration::from_secs_f64(seconds))
    }
}

fn require_machine(config: &Config) {
    if !config.machine {
        usage();
    }
}

fn option_vec(value: Option<String>) -> Vec<String> {
    value.into_iter().collect()
}

fn push_option<T: ToString>(args: &mut Vec<String>, name: &str, value: Option<T>) {
    if let Some(value) = value {
        args.push(name.to_string());
        args.push(value.to_string());
    }
}

fn push_flag(args: &mut Vec<String>, name: &str, present: bool) {
    if present {
        args.push(name.to_string());
    }
}

impl AuthKeygenArgs {
    fn into_args(self) -> Vec<String> {
        vec![
            "--private".to_string(),
            self.private,
            "--public".to_string(),
            self.public,
            "--private-access".to_string(),
            self.private_access,
        ]
    }
}

impl CertArgs {
    fn into_args(self) -> Vec<String> {
        match self.command {
            CertCommand::Root(args) => {
                vec![
                    "root".to_string(),
                    "--private".to_string(),
                    args.private,
                    "--public".to_string(),
                    args.public,
                ]
            }
            CertCommand::Sign(args) => args.into_args(),
            CertCommand::Print(args) => {
                let mut values = vec!["print".to_string(), "--path".to_string(), args.path];
                push_option(&mut values, "--at", args.at);
                values
            }
            CertCommand::Verify(args) => args.into_args(),
        }
    }
}

impl CertSignArgs {
    fn into_args(self) -> Vec<String> {
        let mut args = vec![
            "sign".to_string(),
            "--root-private".to_string(),
            self.root_private,
            "--name".to_string(),
            self.name,
        ];
        push_option(&mut args, "--key", self.key);
        push_option(&mut args, "--key-file", self.key_file);
        for group in self.group {
            push_option(&mut args, "--group", Some(group));
        }
        push_option(&mut args, "--days", self.days);
        push_option(&mut args, "--not-before", self.not_before);
        push_option(&mut args, "--not-after", self.not_after);
        push_option(&mut args, "--out", self.out);
        args
    }
}

impl CertVerifyArgs {
    fn into_args(self) -> Vec<String> {
        let mut args = vec!["verify".to_string(), "--path".to_string(), self.path];
        push_option(&mut args, "--root", self.root);
        push_option(&mut args, "--root-file", self.root_file);
        push_option(&mut args, "--at", self.at);
        args
    }
}

impl WriteArgs {
    fn into_args(self) -> Vec<String> {
        let mut args = Vec::new();
        push_flag(&mut args, "-l", self.line);
        args.push(self.path);
        args.extend(self.offset);
        args.extend(self.data_hex);
        args
    }
}

impl ScriptArgs {
    fn into_args(self) -> Vec<String> {
        let mut args = vec![self.first];
        args.extend(self.second);
        args
    }
}

impl RpcArgs {
    fn into_args(self) -> Vec<String> {
        let mut args = vec![self.path];
        args.extend(self.request);
        args
    }
}

impl LsArgs {
    fn into_args(self) -> Vec<String> {
        let mut args = Vec::new();
        push_flag(&mut args, "-l", self.long);
        push_flag(&mut args, "-d", self.directory);
        push_flag(&mut args, "-n", self.no_sort);
        push_flag(&mut args, "-t", self.sort_time);
        args.extend(self.paths);
        args
    }
}

impl ConArgs {
    fn into_args(self) -> Vec<String> {
        let mut args = Vec::new();
        push_flag(&mut args, "--resume", self.resume);
        push_flag(&mut args, "-r", self.preserve_cr);
        args.push(self.path);
        args
    }
}

impl SessionArgs {
    fn into_args(self) -> Vec<String> {
        match self.command {
            SessionCommand::Serve(args) => args.into_args(),
            SessionCommand::Status(args) => {
                vec!["status".to_string(), "--socket".to_string(), args.socket]
            }
            SessionCommand::Snapshot(args) => args.into_args(),
            SessionCommand::Stat(args) => args.into_args("stat"),
            SessionCommand::List(args) => args.into_args("list"),
            SessionCommand::Read(args) => vec![
                "read".to_string(),
                "--socket".to_string(),
                args.socket,
                args.path,
            ],
        }
    }
}

impl SessionServeArgs {
    fn into_args(self) -> Vec<String> {
        let mut args = vec!["serve".to_string(), "--socket".to_string(), self.socket];
        push_option(&mut args, "--change-feed", self.change_feed);
        push_option(&mut args, "--change-feed-stream", self.change_feed_stream);
        push_option(
            &mut args,
            "--change-feed-cursor-template",
            self.change_feed_cursor_template,
        );
        push_option(
            &mut args,
            "--change-feed-reconnect-delay",
            self.change_feed_reconnect_delay,
        );
        push_option(
            &mut args,
            "--change-feed-backpressure",
            self.change_feed_backpressure,
        );
        push_option(&mut args, "--mount", self.mount);
        push_option(&mut args, "--mount-source", self.mount_source);
        push_option(&mut args, "--mount-status-file", self.mount_status_file);
        push_option(
            &mut args,
            "--mount-diagnostics-file",
            self.mount_diagnostics_file,
        );
        push_option(&mut args, "--mount-attr-timeout", self.mount_attr_timeout);
        push_option(&mut args, "--mount-entry-timeout", self.mount_entry_timeout);
        push_option(
            &mut args,
            "--mount-negative-timeout",
            self.mount_negative_timeout,
        );
        push_flag(
            &mut args,
            "--mount-coherent-read-cache",
            self.mount_coherent_read_cache,
        );
        args.extend(self.endpoint);
        args
    }
}

impl SessionSnapshotArgs {
    fn into_args(self) -> Vec<String> {
        let mut args = vec!["snapshot".to_string(), "--socket".to_string(), self.socket];
        push_option(&mut args, "--depth", self.depth);
        args.extend(self.path);
        args
    }
}

impl SessionOptionalPathArgs {
    fn into_args(self, command: &str) -> Vec<String> {
        let mut args = vec![command.to_string(), "--socket".to_string(), self.socket];
        args.extend(self.path);
        args
    }
}

impl MountArgs {
    fn into_args(self) -> Vec<String> {
        let action = matches!(
            self.endpoint_or_action.as_str(),
            "ensure" | "read-ahead" | "status" | "stop"
        );
        let mut options = Vec::new();
        for endpoint in self.fallback_endpoint {
            options.push("--fallback-endpoint".to_string());
            options.push(endpoint);
        }
        push_option(&mut options, "--source", self.source);
        push_option(&mut options, "--aname", self.aname);
        push_option(&mut options, "--uname", self.uname);
        push_option(&mut options, "--msize", self.msize);
        push_flag(&mut options, "--debug", self.debug);
        push_flag(&mut options, "--allow-other", self.allow_other);
        push_flag(
            &mut options,
            "--coherent-read-cache",
            self.coherent_read_cache,
        );
        push_option(&mut options, "--attr-timeout", self.attr_timeout);
        push_option(&mut options, "--entry-timeout", self.entry_timeout);
        push_option(&mut options, "--negative-timeout", self.negative_timeout);
        push_option(&mut options, "--request-timeout", self.request_timeout);
        push_option(&mut options, "--connect-timeout", self.connect_timeout);
        push_option(&mut options, "--lookup-timeout", self.lookup_timeout);
        push_option(&mut options, "--read-timeout", self.read_timeout);
        push_option(
            &mut options,
            "--change-feed-read-timeout",
            self.change_feed_read_timeout,
        );
        push_option(&mut options, "--write-timeout", self.write_timeout);
        push_option(&mut options, "--mutation-timeout", self.mutation_timeout);
        push_option(&mut options, "--control-timeout", self.control_timeout);
        push_option(&mut options, "--interrupt-timeout", self.interrupt_timeout);
        push_option(&mut options, "--max-workers", self.max_workers);
        push_option(&mut options, "--max-background", self.max_background);
        push_option(
            &mut options,
            "--congestion-threshold",
            self.congestion_threshold,
        );
        push_option(&mut options, "--diagnostics-file", self.diagnostics_file);
        push_option(
            &mut options,
            "--diagnostics-capacity",
            self.diagnostics_capacity,
        );
        push_option(&mut options, "--status-file", self.status_file);
        push_option(&mut options, "--change-feed", self.change_feed);
        push_option(
            &mut options,
            "--change-feed-stream",
            self.change_feed_stream,
        );
        push_option(
            &mut options,
            "--change-feed-cursor-template",
            self.change_feed_cursor_template,
        );
        push_option(&mut options, "--change-feed-scope", self.change_feed_scope);
        push_option(
            &mut options,
            "--change-feed-reconnect-delay",
            self.change_feed_reconnect_delay,
        );
        push_option(
            &mut options,
            "--change-feed-backpressure",
            self.change_feed_backpressure,
        );
        push_option(&mut options, "--mountpoint", self.supervised_mountpoint);
        push_option(&mut options, "--unit", self.unit);
        push_option(&mut options, "--unit-scope", self.unit_scope);
        push_option(&mut options, "--expect-endpoint", self.expect_endpoint);
        push_option(
            &mut options,
            "--expect-status-file",
            self.expect_status_file,
        );
        push_option(
            &mut options,
            "--expect-change-feed",
            self.expect_change_feed,
        );
        push_option(&mut options, "--attempts", self.attempts);
        push_option(&mut options, "--kilobytes", self.read_ahead_kilobytes);

        let mut args = Vec::new();
        if action {
            args.push(self.endpoint_or_action);
            args.extend(options);
        } else {
            args.extend(options);
            args.push(self.endpoint_or_action);
        }
        args.extend(self.direct_mountpoint);
        if !self.mount_args.is_empty() {
            args.push("--".to_string());
            args.extend(self.mount_args);
        }
        args
    }
}

impl ServeArgs {
    fn into_args(self) -> Vec<String> {
        let mut args = Vec::new();
        push_option(&mut args, "--bind", self.bind);
        push_option(&mut args, "--max-fids", self.max_fids);
        push_flag(&mut args, "--writable", self.writable);
        args.push(self.root);
        args
    }
}

impl ExportArgs {
    fn into_args(self) -> Vec<String> {
        let mut args = Vec::new();
        push_option(&mut args, "--bind", self.bind);
        push_option(&mut args, "--max-fids", self.max_fids);
        push_flag(&mut args, "--writable", self.writable);
        push_option(&mut args, "--descriptor", Some(self.descriptor));
        push_option(&mut args, "--descriptor-file", self.descriptor_file);
        push_option(&mut args, "--auth-config", self.auth_config);
        for field in self.descriptor_field {
            push_option(&mut args, "--descriptor-field", Some(field));
        }
        args.push(self.root);
        args
    }
}

impl StreamExportArgs {
    fn into_args(self) -> Vec<String> {
        let mut args = vec!["--bind".to_string(), self.bind];
        args.extend(["--auth-config".to_string(), self.auth_config]);
        for principal in self.allow_principal {
            args.extend(["--allow-principal".to_string(), principal]);
        }
        push_option(&mut args, "--max-sessions", self.max_sessions);
        push_option(&mut args, "--max-buffer-bytes", self.max_buffer_bytes);
        push_option(&mut args, "--status-file", self.status_file);
        args.push("--".to_string());
        args.extend(self.command);
        args
    }
}

impl ReverseBrokerArgs {
    fn into_args(self) -> Vec<String> {
        let mut args = vec!["--reverse-bind".to_string(), self.reverse_bind];
        push_option(&mut args, "--proxy-bind", self.proxy_bind);
        push_option(&mut args, "--proxy-exposure", self.proxy_exposure);
        push_option(&mut args, "--auth-config", self.auth_config);
        push_option(&mut args, "--principal", Some(self.principal));
        push_option(&mut args, "--pool", self.pool);
        push_option(&mut args, "--auth-timeout", self.auth_timeout);
        push_option(&mut args, "--proxy-wait-timeout", self.proxy_wait_timeout);
        args
    }
}

impl ReverseExportArgs {
    fn into_args(self) -> Vec<String> {
        let mut args = vec!["--connect".to_string(), self.connect];
        push_option(&mut args, "--auth-config", self.auth_config);
        push_option(&mut args, "--principal", Some(self.principal));
        push_option(&mut args, "--pool", self.pool);
        push_option(&mut args, "--max-fids", self.max_fids);
        push_option(&mut args, "--connect-timeout", self.connect_timeout);
        push_option(&mut args, "--auth-timeout", self.auth_timeout);
        push_option(&mut args, "--reconnect-min-delay", self.reconnect_min_delay);
        push_option(&mut args, "--reconnect-max-delay", self.reconnect_max_delay);
        push_flag(&mut args, "--writable", self.writable);
        args.push(self.root);
        args
    }
}

impl SessionProxyArgs {
    fn into_args(self) -> Vec<String> {
        let mut args = Vec::new();
        push_option(&mut args, "--bind", self.bind);
        push_option(&mut args, "--connect", Some(self.connect));
        push_option(&mut args, "--auth-config", self.auth_config);
        push_option(&mut args, "--principal", Some(self.principal));
        push_option(&mut args, "--max-sessions", self.max_sessions);
        push_option(&mut args, "--connect-timeout", self.connect_timeout);
        push_option(&mut args, "--auth-timeout", self.auth_timeout);
        args
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_command_tree_is_internally_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn global_options_preserve_plan9port_clusters_and_extensions() {
        let cli = Cli::try_parse_from([
            "r9p",
            "-nD",
            "--machine",
            "-a",
            "tcp!127.0.0.1!9564",
            "-A/",
            "-ucodex",
            "-m65536",
            "--request-timeout",
            "0.25",
            "--control-timeout",
            "12.5",
            "ls",
            "/",
        ])
        .expect("global options");
        let (config, command) = cli.into_parts();
        assert_eq!(config.address.as_deref(), Some("tcp!127.0.0.1!9564"));
        assert_eq!(config.aname, "/");
        assert_eq!(config.uname, "codex");
        assert_eq!(config.msize, 65_536);
        assert!(config.msize_set);
        assert!(config.machine);
        assert_eq!(config.request_timeout, Some(Duration::from_millis(250)));
        assert_eq!(config.control_timeout, Some(Duration::from_millis(12_500)));
        assert!(matches!(command, Command::Ls(_)));
    }

    #[test]
    fn zero_timeout_disables_socket_deadlines() {
        let cli = Cli::try_parse_from(["r9p", "--request-timeout", "0", "read", "/events/stream"])
            .expect("zero timeout");
        assert_eq!(cli.into_parts().0.request_timeout, None);
    }

    #[test]
    fn bind_aliases_and_read_alias_are_generated_by_the_parser() {
        for bind in [
            vec!["--bind", "192.168.0.30:9564"],
            vec!["--bind=192.168.0.30:9564"],
        ] {
            let mut values = vec!["r9p"];
            values.extend(bind);
            values.extend(["cat", "/status"]);
            let cli = Cli::try_parse_from(values).expect("bind and cat aliases");
            let (config, command) = cli.into_parts();
            assert_eq!(config.address.as_deref(), Some("192.168.0.30:9564"));
            assert!(matches!(command, Command::Read(_)));
        }
    }

    #[test]
    fn generated_help_covers_nested_and_complex_commands() {
        let top = Cli::try_parse_from(["r9p", "--help"])
            .expect_err("help exits before dispatch")
            .to_string();
        assert!(top.contains("Operate on 9P services"));
        assert!(top.contains("auth-keygen"));
        assert!(top.contains("mount"));
        assert!(top.contains("stream-export"));

        let cert = Cli::try_parse_from(["r9p", "help", "cert", "sign"])
            .expect_err("nested help exits before dispatch")
            .to_string();
        assert!(cert.contains("--root-private <PATH>"));
        assert!(cert.contains("--key <HEX>"));

        let mount = Cli::try_parse_from(["r9p", "mount", "--help"])
            .expect_err("command help exits before dispatch")
            .to_string();
        assert!(mount.contains("--change-feed-cursor-template"));
        assert!(mount.contains("--mountpoint <PATH>"));
    }

    #[test]
    fn generated_parser_normalizes_nested_session_and_mount_invocations() {
        let session = Cli::try_parse_from([
            "r9p",
            "session",
            "snapshot",
            "--socket",
            "/run/r9p.sock",
            "--depth",
            "3",
            "/memory",
        ])
        .expect("session snapshot");
        let Command::Session(session) = session.command else {
            panic!("expected session command");
        };
        assert_eq!(
            session.into_args(),
            [
                "snapshot",
                "--socket",
                "/run/r9p.sock",
                "--depth",
                "3",
                "/memory",
            ]
        );

        let mount = Cli::try_parse_from([
            "r9p",
            "mount",
            "ensure",
            "--mountpoint",
            "/mnt/wiki",
            "--unit",
            "wiki.mount",
            "--unit-scope",
            "system",
            "--",
            "--source",
            "/memory",
            "m7.mesh:9564",
            "/mnt/wiki",
        ])
        .expect("supervised mount");
        let Command::Mount(mount) = mount.command else {
            panic!("expected mount command");
        };
        let normalized = mount.into_args();
        assert_eq!(normalized.first().map(String::as_str), Some("ensure"));
        assert!(normalized.contains(&"--mountpoint".to_string()));

        let read_ahead = Cli::try_parse_from([
            "r9p",
            "mount",
            "read-ahead",
            "--mountpoint",
            "/mnt/wiki",
            "--kilobytes",
            "4096",
        ])
        .expect("read-ahead mount action");
        let Command::Mount(read_ahead) = read_ahead.command else {
            panic!("expected mount command");
        };
        assert_eq!(
            read_ahead.into_args(),
            [
                "read-ahead",
                "--mountpoint",
                "/mnt/wiki",
                "--kilobytes",
                "4096",
            ]
        );

        let direct_mount = Cli::try_parse_from([
            "r9p",
            "mount",
            "--fallback-endpoint",
            "nucbox.mesh:9564",
            "m7.mesh:9564",
            "/mnt/namespace",
        ])
        .expect("direct mount with fallback");
        let Command::Mount(direct_mount) = direct_mount.command else {
            panic!("expected mount command");
        };
        assert_eq!(
            direct_mount.into_args(),
            [
                "--fallback-endpoint",
                "nucbox.mesh:9564",
                "m7.mesh:9564",
                "/mnt/namespace",
            ]
        );
    }
}
