use std::{
    net::{SocketAddr, ToSocketAddrs},
    path::PathBuf,
    thread,
    time::Duration,
};

use r9p_auth::{ClientConfig, ServerConfig};
use r9p_reverse::{
    BrokerConfig, FilesystemExport, FilesystemExportConfig, ProxyEndpoint, ProxyExposure,
    ReverseBroker, SessionProxy, SessionProxyConfig,
};

use crate::{
    errors::{cli_error, CliResult},
    target::Config,
};

const DEFAULT_POOL: usize = 8;
const DEFAULT_MAX_FIDS: usize = 4096;
const DEFAULT_AUTH_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_PROXY_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_RECONNECT_MIN_DELAY: Duration = Duration::from_millis(250);
const DEFAULT_RECONNECT_MAX_DELAY: Duration = Duration::from_secs(30);

pub(crate) fn reverse_broker_cmd(global: Config, args: Vec<String>) -> CliResult<()> {
    let config = parse_broker_config(global, args)?;
    let broker = ReverseBroker::start(BrokerConfig {
        reverse_bind: config.reverse_bind,
        proxy_bind: config.proxy_bind,
        proxy_exposure: config.proxy_exposure,
        auth: ServerConfig::read(&config.auth_config)?,
        peer_principal: config.principal.clone(),
        max_waiting_streams: config.pool,
        authentication_timeout: config.auth_timeout,
        proxy_wait_timeout: config.proxy_wait_timeout,
    })?;
    println!("reverse_endpoint\t{}", broker.reverse_endpoint());
    println!("proxy_endpoint\t{}", broker.proxy_endpoint());
    println!("principal\t{}", config.principal);
    park_forever();
}

pub(crate) fn reverse_export_cmd(global: Config, args: Vec<String>) -> CliResult<()> {
    let config = parse_export_config(global, args)?;
    let export = FilesystemExport::start(FilesystemExportConfig {
        broker_endpoint: config.broker_endpoint,
        auth: ClientConfig::read(&config.auth_config)?,
        expected_responder: config.expected_responder.clone(),
        principal: config.principal,
        root: config.root,
        writable: config.writable,
        connection_pool: config.pool,
        connect_timeout: config.connect_timeout,
        authentication_timeout: config.auth_timeout,
        reconnect_min_delay: config.reconnect_min_delay,
        reconnect_max_delay: config.reconnect_max_delay,
        msize: config.msize,
        max_fids: config.max_fids,
    })?;
    let _export = export;
    park_forever();
}

pub(crate) fn session_proxy_cmd(global: Config, args: Vec<String>) -> CliResult<()> {
    let config = parse_session_proxy_config(global, args)?;
    let proxy = SessionProxy::start(SessionProxyConfig {
        bind: config.bind,
        upstream: config.upstream,
        auth: ClientConfig::read(&config.auth_config)?,
        principal: config.principal.clone(),
        max_sessions: config.max_sessions,
        connect_timeout: config.connect_timeout,
        authentication_timeout: config.auth_timeout,
    })?;
    println!("proxy_endpoint\t{}", proxy.endpoint());
    println!("upstream_endpoint\t{}", config.upstream);
    println!("principal\t{}", config.principal);
    park_forever();
}

#[derive(Debug, Eq, PartialEq)]
struct ReverseBrokerCliConfig {
    reverse_bind: SocketAddr,
    proxy_bind: ProxyEndpoint,
    proxy_exposure: ProxyExposure,
    auth_config: PathBuf,
    principal: String,
    pool: usize,
    auth_timeout: Duration,
    proxy_wait_timeout: Duration,
}

#[derive(Debug, Eq, PartialEq)]
struct ReverseExportCliConfig {
    broker_endpoint: SocketAddr,
    auth_config: PathBuf,
    expected_responder: String,
    principal: String,
    root: PathBuf,
    writable: bool,
    pool: usize,
    connect_timeout: Duration,
    auth_timeout: Duration,
    reconnect_min_delay: Duration,
    reconnect_max_delay: Duration,
    msize: u32,
    max_fids: usize,
}

#[derive(Debug, Eq, PartialEq)]
struct SessionProxyCliConfig {
    bind: ProxyEndpoint,
    upstream: SocketAddr,
    auth_config: PathBuf,
    expected_responder: String,
    principal: String,
    max_sessions: usize,
    connect_timeout: Duration,
    auth_timeout: Duration,
}

fn parse_broker_config(global: Config, args: Vec<String>) -> CliResult<ReverseBrokerCliConfig> {
    reject_client_globals(&global, "reverse-broker")?;
    let mut reverse_bind = None;
    let mut proxy_bind = Some(ProxyEndpoint::tcp(loopback_ephemeral()));
    let mut proxy_exposure = ProxyExposure::Local;
    let mut auth_config = global.auth_config;
    let mut principal = None;
    let mut pool = DEFAULT_POOL;
    let mut auth_timeout = DEFAULT_AUTH_TIMEOUT;
    let mut proxy_wait_timeout = DEFAULT_PROXY_WAIT_TIMEOUT;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--reverse-bind" => {
                reverse_bind = Some(parse_socket(value(&args, &mut index, "reverse bind")?)?);
            }
            "--proxy-bind" => {
                proxy_bind = Some(parse_proxy_endpoint(value(
                    &args,
                    &mut index,
                    "proxy bind",
                )?)?);
            }
            "--proxy-exposure" => {
                proxy_exposure = parse_proxy_exposure(value(&args, &mut index, "proxy exposure")?)?;
            }
            "--auth-config" => {
                set_auth_config(&mut auth_config, value(&args, &mut index, "auth config")?)?;
            }
            "--principal" => {
                set_once(
                    &mut principal,
                    value(&args, &mut index, "principal")?,
                    "principal",
                )?;
            }
            "--pool" => pool = parse_positive(value(&args, &mut index, "pool")?, "pool")?,
            "--auth-timeout" => {
                auth_timeout = parse_duration(value(&args, &mut index, "auth timeout")?)?
            }
            "--proxy-wait-timeout" => {
                proxy_wait_timeout =
                    parse_duration(value(&args, &mut index, "proxy wait timeout")?)?
            }
            "-h" | "--help" => reverse_usage(0),
            option => return Err(cli_error(format!("unknown reverse-broker option {option}"))),
        }
        index += 1;
    }
    let reverse_bind = reverse_bind.ok_or_else(|| cli_error("missing --reverse-bind"))?;
    let proxy_bind = proxy_bind.ok_or_else(|| cli_error("missing --proxy-bind"))?;
    validate_proxy_exposure(&proxy_bind, proxy_exposure)?;
    Ok(ReverseBrokerCliConfig {
        reverse_bind,
        proxy_bind,
        proxy_exposure,
        auth_config: auth_config.ok_or_else(|| cli_error("missing --auth-config"))?,
        principal: principal.ok_or_else(|| cli_error("missing --principal"))?,
        pool,
        auth_timeout,
        proxy_wait_timeout,
    })
}

fn parse_export_config(global: Config, args: Vec<String>) -> CliResult<ReverseExportCliConfig> {
    let auth_domain = global.auth_domain.clone();
    reject_client_globals(&global, "reverse-export")?;
    let mut broker_endpoint = None;
    let mut auth_config = global.auth_config;
    let mut principal = None;
    let mut root = None;
    let mut writable = false;
    let mut pool = DEFAULT_POOL;
    let mut connect_timeout = DEFAULT_CONNECT_TIMEOUT;
    let mut auth_timeout = DEFAULT_AUTH_TIMEOUT;
    let mut reconnect_min_delay = DEFAULT_RECONNECT_MIN_DELAY;
    let mut reconnect_max_delay = DEFAULT_RECONNECT_MAX_DELAY;
    let mut max_fids = DEFAULT_MAX_FIDS;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--connect" => {
                broker_endpoint = Some(parse_socket(value(&args, &mut index, "broker")?)?);
            }
            "--auth-config" => {
                set_auth_config(&mut auth_config, value(&args, &mut index, "auth config")?)?;
            }
            "--principal" => {
                set_once(
                    &mut principal,
                    value(&args, &mut index, "principal")?,
                    "principal",
                )?;
            }
            "--pool" => pool = parse_positive(value(&args, &mut index, "pool")?, "pool")?,
            "--max-fids" => {
                max_fids = parse_positive(value(&args, &mut index, "max fids")?, "max fids")?
            }
            "--connect-timeout" => {
                connect_timeout = parse_duration(value(&args, &mut index, "connect timeout")?)?
            }
            "--auth-timeout" => {
                auth_timeout = parse_duration(value(&args, &mut index, "auth timeout")?)?
            }
            "--reconnect-min-delay" => {
                reconnect_min_delay =
                    parse_duration(value(&args, &mut index, "reconnect minimum delay")?)?
            }
            "--reconnect-max-delay" => {
                reconnect_max_delay =
                    parse_duration(value(&args, &mut index, "reconnect maximum delay")?)?
            }
            "--writable" => writable = true,
            "-h" | "--help" => reverse_usage(0),
            option if option.starts_with('-') => {
                return Err(cli_error(format!("unknown reverse-export option {option}")));
            }
            value => {
                if root.replace(PathBuf::from(value)).is_some() {
                    return Err(cli_error("reverse-export expects one root directory"));
                }
            }
        }
        index += 1;
    }
    Ok(ReverseExportCliConfig {
        broker_endpoint: broker_endpoint.ok_or_else(|| cli_error("missing --connect"))?,
        auth_config: auth_config.ok_or_else(|| cli_error("missing --auth-config"))?,
        expected_responder: auth_domain.ok_or_else(|| cli_error("missing --auth-domain"))?,
        principal: principal.ok_or_else(|| cli_error("missing --principal"))?,
        root: root.ok_or_else(|| cli_error("missing root directory"))?,
        writable,
        pool,
        connect_timeout,
        auth_timeout,
        reconnect_min_delay,
        reconnect_max_delay,
        msize: global.msize,
        max_fids,
    })
}

fn parse_session_proxy_config(
    global: Config,
    args: Vec<String>,
) -> CliResult<SessionProxyCliConfig> {
    reject_client_globals(&global, "session-proxy")?;
    let auth_domain = global.auth_domain.clone();
    let mut bind = Some(ProxyEndpoint::tcp(loopback_ephemeral()));
    let mut upstream = None;
    let mut auth_config = global.auth_config;
    let mut principal = None;
    let mut max_sessions = DEFAULT_POOL;
    let mut connect_timeout = DEFAULT_CONNECT_TIMEOUT;
    let mut auth_timeout = DEFAULT_AUTH_TIMEOUT;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--bind" => {
                bind = Some(parse_proxy_endpoint(value(&args, &mut index, "bind")?)?);
            }
            "--connect" => {
                upstream = Some(parse_socket(value(&args, &mut index, "upstream")?)?);
            }
            "--auth-config" => {
                set_auth_config(&mut auth_config, value(&args, &mut index, "auth config")?)?;
            }
            "--principal" => {
                set_once(
                    &mut principal,
                    value(&args, &mut index, "principal")?,
                    "principal",
                )?;
            }
            "--max-sessions" => {
                max_sessions = parse_positive(
                    value(&args, &mut index, "maximum sessions")?,
                    "maximum sessions",
                )?
            }
            "--connect-timeout" => {
                connect_timeout = parse_duration(value(&args, &mut index, "connect timeout")?)?
            }
            "--auth-timeout" => {
                auth_timeout = parse_duration(value(&args, &mut index, "auth timeout")?)?
            }
            "-h" | "--help" => reverse_usage(0),
            option => return Err(cli_error(format!("unknown session-proxy option {option}"))),
        }
        index += 1;
    }
    let bind = bind.ok_or_else(|| cli_error("missing --bind"))?;
    validate_local_proxy_endpoint(&bind)?;
    Ok(SessionProxyCliConfig {
        bind,
        upstream: upstream.ok_or_else(|| cli_error("missing --connect"))?,
        auth_config: auth_config.ok_or_else(|| cli_error("missing --auth-config"))?,
        expected_responder: auth_domain.ok_or_else(|| cli_error("missing --auth-domain"))?,
        principal: principal.ok_or_else(|| cli_error("missing --principal"))?,
        max_sessions,
        connect_timeout,
        auth_timeout,
    })
}

fn reject_client_globals(global: &Config, command: &str) -> CliResult<()> {
    if global.address.is_some() || !global.aname.is_empty() || global.machine {
        return Err(cli_error(format!(
            "r9p {command} does not accept client address, aname, or machine options"
        )));
    }
    Ok(())
}

fn value<'a>(args: &'a [String], index: &mut usize, label: &str) -> CliResult<&'a str> {
    *index += 1;
    args.get(*index)
        .map(String::as_str)
        .ok_or_else(|| cli_error(format!("missing {label}")))
}

fn set_auth_config(target: &mut Option<PathBuf>, value: &str) -> CliResult<()> {
    if target.replace(PathBuf::from(value)).is_some() {
        Err(cli_error("auth config already specified"))
    } else {
        Ok(())
    }
}

fn set_once(target: &mut Option<String>, value: &str, label: &str) -> CliResult<()> {
    if target.replace(value.to_string()).is_some() {
        Err(cli_error(format!("{label} already specified")))
    } else {
        Ok(())
    }
}

fn parse_socket(value: &str) -> CliResult<SocketAddr> {
    value
        .to_socket_addrs()
        .map_err(|error| cli_error(format!("invalid socket address {value}: {error}")))?
        .next()
        .ok_or_else(|| cli_error(format!("socket address {value} resolved no addresses")))
}

fn parse_proxy_endpoint(value: &str) -> CliResult<ProxyEndpoint> {
    if let Some(path) = value
        .strip_prefix("unix!")
        .or_else(|| value.strip_prefix("unix:"))
    {
        let path = PathBuf::from(path);
        if !path.is_absolute() || path.file_name().is_none() {
            return Err(cli_error("reverse Unix proxy path must be absolute"));
        }
        return Ok(ProxyEndpoint::unix(path));
    }
    parse_socket(value).map(ProxyEndpoint::tcp)
}

fn parse_proxy_exposure(value: &str) -> CliResult<ProxyExposure> {
    match value {
        "local" => Ok(ProxyExposure::Local),
        "authenticated-network" => Ok(ProxyExposure::AuthenticatedNetwork),
        _ => Err(cli_error(format!("invalid proxy exposure {value}"))),
    }
}

fn validate_proxy_exposure(endpoint: &ProxyEndpoint, exposure: ProxyExposure) -> CliResult<()> {
    let valid = match (endpoint, exposure) {
        (ProxyEndpoint::Tcp(address), ProxyExposure::Local) => address.ip().is_loopback(),
        (ProxyEndpoint::Unix(_), ProxyExposure::Local) => true,
        (ProxyEndpoint::Tcp(address), ProxyExposure::AuthenticatedNetwork) => {
            !address.ip().is_loopback()
                && !address.ip().is_unspecified()
                && !address.ip().is_multicast()
                && address.port() != 0
        }
        (ProxyEndpoint::Unix(_), ProxyExposure::AuthenticatedNetwork) => false,
    };
    if valid {
        Ok(())
    } else {
        Err(cli_error(
            "proxy endpoint does not match its declared exposure",
        ))
    }
}

fn validate_local_proxy_endpoint(endpoint: &ProxyEndpoint) -> CliResult<()> {
    match endpoint {
        ProxyEndpoint::Tcp(address) if !address.ip().is_loopback() => {
            Err(cli_error("session proxy TCP bind must be loopback"))
        }
        _ => Ok(()),
    }
}

fn parse_positive(value: &str, label: &str) -> CliResult<usize> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| cli_error(format!("invalid {label} {value}")))?;
    if parsed == 0 {
        Err(cli_error(format!("{label} must be positive")))
    } else {
        Ok(parsed)
    }
}

fn parse_duration(value: &str) -> CliResult<Duration> {
    let seconds = value
        .parse::<f64>()
        .map_err(|_| cli_error(format!("invalid duration {value}")))?;
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err(cli_error(format!("duration must be positive: {value}")));
    }
    Ok(Duration::from_secs_f64(seconds))
}

fn loopback_ephemeral() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 0))
}

fn park_forever() -> ! {
    loop {
        thread::park();
    }
}

fn reverse_usage(code: i32) -> ! {
    eprintln!(
        "usage: r9p reverse-broker --reverse-bind address [--proxy-bind address|unix!/path] [--proxy-exposure local|authenticated-network] --principal name --auth-config path [--pool count]"
    );
    eprintln!(
        "       r9p reverse-export --connect address --principal name --auth-config path [--pool count] [--reconnect-min-delay seconds] [--reconnect-max-delay seconds] [--writable] root"
    );
    eprintln!(
        "       r9p session-proxy --bind loopback-address|unix!/path --connect address --principal name --auth-config path [--max-sessions count]"
    );
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn global() -> Config {
        Config {
            auth_domain: None,
            address: None,
            auth_config: None,
            aname: String::new(),
            uname: "tester".to_string(),
            msize: 65_536,
            msize_set: false,
            machine: false,
            request_timeout: Some(Duration::from_secs(30)),
            control_timeout: Some(Duration::from_secs(600)),
        }
    }

    #[test]
    fn parses_reverse_broker() {
        let parsed = parse_broker_config(
            global(),
            [
                "--reverse-bind",
                "0.0.0.0:9640",
                "--proxy-bind",
                "127.0.0.1:9641",
                "--principal",
                "laptop-workspace",
                "--auth-config",
                "/run/auth/server.conf",
                "--pool",
                "4",
            ]
            .map(str::to_string)
            .to_vec(),
        )
        .expect("broker config");
        assert_eq!(parsed.reverse_bind, SocketAddr::from(([0, 0, 0, 0], 9640)));
        assert_eq!(
            parsed.proxy_bind,
            ProxyEndpoint::tcp(SocketAddr::from(([127, 0, 0, 1], 9641)))
        );
        assert_eq!(parsed.proxy_exposure, ProxyExposure::Local);
        assert_eq!(parsed.pool, 4);
    }

    #[test]
    fn parses_unix_reverse_proxy() {
        let parsed = parse_broker_config(
            global(),
            [
                "--reverse-bind",
                "0.0.0.0:9640",
                "--proxy-bind",
                "unix!/run/r9p/operating.sock",
                "--principal",
                "laptop-workspace",
                "--auth-config",
                "/run/auth/server.conf",
            ]
            .map(str::to_string)
            .to_vec(),
        )
        .expect("broker config");
        assert_eq!(
            parsed.proxy_bind,
            ProxyEndpoint::unix("/run/r9p/operating.sock")
        );
    }

    #[test]
    fn parses_reverse_export() {
        let parsed = parse_export_config(
            global(),
            [
                "--connect",
                "192.168.0.30:9640",
                "--principal",
                "laptop-workspace",
                "--auth-config",
                "/run/auth/client.conf",
                "--writable",
                "/home/test/workspace",
            ]
            .map(str::to_string)
            .to_vec(),
        )
        .expect("export config");
        assert_eq!(
            parsed.broker_endpoint,
            SocketAddr::from(([192, 168, 0, 30], 9640))
        );
        assert!(parsed.writable);
        assert_eq!(parsed.root, PathBuf::from("/home/test/workspace"));
    }

    #[test]
    fn parses_session_proxy() {
        let parsed = parse_session_proxy_config(
            global(),
            [
                "--bind",
                "127.0.0.6:9671",
                "--connect",
                "127.0.0.1:9641",
                "--principal",
                "/srv/example/runtime/m7",
                "--auth-config",
                "/run/auth/client.conf",
                "--max-sessions",
                "4",
            ]
            .map(str::to_string)
            .to_vec(),
        )
        .expect("session proxy config");
        assert_eq!(
            parsed.bind,
            ProxyEndpoint::tcp(SocketAddr::from(([127, 0, 0, 6], 9671)))
        );
        assert_eq!(parsed.upstream, SocketAddr::from(([127, 0, 0, 1], 9641)));
        assert_eq!(parsed.principal, "/srv/example/runtime/m7");
        assert_eq!(parsed.max_sessions, 4);
    }

    #[test]
    fn rejects_non_loopback_proxy() {
        let result = parse_broker_config(
            global(),
            [
                "--reverse-bind",
                "0.0.0.0:9640",
                "--proxy-bind",
                "0.0.0.0:9641",
                "--principal",
                "laptop-workspace",
                "--auth-config",
                "/run/auth/server.conf",
            ]
            .map(str::to_string)
            .to_vec(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn parses_authenticated_network_reverse_proxy() {
        let parsed = parse_broker_config(
            global(),
            [
                "--reverse-bind",
                "0.0.0.0:9640",
                "--proxy-bind",
                "192.168.0.30:9641",
                "--proxy-exposure",
                "authenticated-network",
                "--principal",
                "laptop-workspace",
                "--auth-config",
                "/run/auth/server.conf",
            ]
            .map(str::to_string)
            .to_vec(),
        )
        .expect("network broker config");
        assert_eq!(
            parsed.proxy_bind,
            ProxyEndpoint::tcp(SocketAddr::from(([192, 168, 0, 30], 9641)))
        );
        assert_eq!(parsed.proxy_exposure, ProxyExposure::AuthenticatedNetwork);
    }
}
