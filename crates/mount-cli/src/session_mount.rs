use std::{
    path::{Path, PathBuf},
    thread::{self, JoinHandle},
    time::Duration,
};

use cli::{cli_error, CliResult};
use fuse::Config as MountConfig;
use session::control::{ControlConfig, ControlRuntime};

#[derive(Debug, Clone)]
pub(crate) struct SessionMountConfig {
    mountpoint: Option<PathBuf>,
    source_path: String,
    status_path: Option<PathBuf>,
    diagnostics_path: Option<PathBuf>,
    attr_timeout: Duration,
    entry_timeout: Duration,
    negative_timeout: Duration,
    coherent_read_cache: bool,
    read_cache_path: Option<PathBuf>,
    read_cache_max_bytes: u64,
}

pub(crate) fn take_session_mount_config(args: &mut Vec<String>) -> CliResult<SessionMountConfig> {
    let mountpoint = take_optional_value(args, "--mount")?.map(PathBuf::from);
    let source_path =
        take_optional_value(args, "--mount-source")?.unwrap_or_else(|| "/".to_string());
    let status_path = take_optional_value(args, "--mount-status-file")?.map(PathBuf::from);
    let diagnostics_path =
        take_optional_value(args, "--mount-diagnostics-file")?.map(PathBuf::from);
    let attr_timeout = take_optional_value(args, "--mount-attr-timeout")?
        .map(|value| parse_duration_secs(&value, "mount attr timeout"))
        .transpose()?
        .unwrap_or(fuse::DEFAULT_ATTR_TIMEOUT);
    let entry_timeout = take_optional_value(args, "--mount-entry-timeout")?
        .map(|value| parse_duration_secs(&value, "mount entry timeout"))
        .transpose()?
        .unwrap_or(fuse::DEFAULT_ENTRY_TIMEOUT);
    let negative_timeout = take_optional_value(args, "--mount-negative-timeout")?
        .map(|value| parse_duration_secs(&value, "mount negative timeout"))
        .transpose()?
        .unwrap_or(fuse::DEFAULT_NEGATIVE_TIMEOUT);
    let coherent_read_cache = take_flag(args, "--mount-coherent-read-cache");
    let read_cache_path = take_optional_value(args, "--mount-read-cache")?.map(PathBuf::from);
    let read_cache_max_bytes = take_optional_value(args, "--mount-read-cache-max-bytes")?
        .map(|value| parse_read_cache_quota(&value))
        .transpose()?
        .unwrap_or(0);
    Ok(SessionMountConfig {
        mountpoint,
        source_path,
        status_path,
        diagnostics_path,
        attr_timeout,
        entry_timeout,
        negative_timeout,
        coherent_read_cache,
        read_cache_path,
        read_cache_max_bytes,
    })
}

pub(crate) fn start_session_mount(
    control: &ControlConfig,
    runtime: &ControlRuntime,
    mount: &SessionMountConfig,
) -> CliResult<Option<JoinHandle<()>>> {
    let Some(mountpoint) = &mount.mountpoint else {
        return Ok(None);
    };
    let feed_events = if control.change_feed_path.is_some() {
        Some(runtime.feed_events().subscribe())
    } else {
        None
    };
    let config = mount_config(control, mountpoint, mount);
    let client = runtime.client_session();
    thread::Builder::new()
        .name("r9p-session-fuse".to_string())
        .spawn(move || {
            if let Err(error) = fuse::mount_with_session(config, client, feed_events) {
                eprintln!("r9p session mount: {}", error.message());
            }
        })
        .map(Some)
        .map_err(|error| cli_error(format!("spawn session mount: {error}")))
}

fn mount_config(
    control: &ControlConfig,
    mountpoint: &Path,
    mount: &SessionMountConfig,
) -> MountConfig {
    MountConfig {
        address: control.address.clone(),
        fallback_addresses: Vec::new(),
        authentication: control.authentication.clone(),
        source_path: mount.source_path.clone(),
        mountpoint: mountpoint.to_string_lossy().into_owned(),
        uname: control.uname.clone(),
        aname: control.aname.clone(),
        msize: control.msize,
        connect_timeout: control.connect_timeout,
        attr_timeout: mount.attr_timeout,
        entry_timeout: mount.entry_timeout,
        negative_timeout: mount.negative_timeout,
        request_timeout: control.request_timeout,
        lookup_timeout: Duration::ZERO,
        read_timeout: Duration::ZERO,
        change_feed_read_timeout: Duration::ZERO,
        write_timeout: Duration::ZERO,
        mutation_timeout: Duration::ZERO,
        control_timeout: Duration::ZERO,
        interrupt_timeout: Duration::ZERO,
        max_workers: fuse::DEFAULT_MAX_WORKERS,
        max_background: fuse::DEFAULT_MAX_BACKGROUND,
        congestion_threshold: fuse::default_congestion_threshold(fuse::DEFAULT_MAX_BACKGROUND),
        diagnostics_path: mount.diagnostics_path.clone(),
        diagnostics_capacity: 0,
        status_path: mount.status_path.clone(),
        change_feed_path: None,
        change_feed_stream_path: None,
        change_feed_cursor_template: None,
        change_feed_scope: None,
        change_feed_reconnect_delay: Duration::ZERO,
        change_feed_backpressure_limit: 0,
        coherent_read_cache: mount.coherent_read_cache,
        read_cache_path: mount.read_cache_path.clone(),
        read_cache_max_bytes: mount.read_cache_max_bytes,
        allow_other: false,
        debug: false,
    }
}

fn take_flag(args: &mut Vec<String>, name: &str) -> bool {
    let present = args.iter().any(|arg| arg == name);
    args.retain(|arg| arg != name);
    present
}

fn parse_duration_secs(value: &str, name: &str) -> CliResult<Duration> {
    let seconds = value
        .parse::<f64>()
        .map_err(|_| cli_error(format!("invalid {name} {value}")))?;
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(cli_error(format!("invalid {name} {value}")));
    }
    Ok(Duration::from_secs_f64(seconds))
}

fn parse_read_cache_quota(value: &str) -> CliResult<u64> {
    let bytes = value
        .parse::<u64>()
        .map_err(|_| cli_error(format!("invalid persistent read cache quota {value}")))?;
    if bytes == 0 || bytes > fuse::MAX_PERSISTENT_READ_CACHE_BYTES {
        return Err(cli_error(format!(
            "persistent read cache quota must be between 1 and {}: {value}",
            fuse::MAX_PERSISTENT_READ_CACHE_BYTES
        )));
    }
    Ok(bytes)
}

fn take_optional_value(args: &mut Vec<String>, name: &str) -> CliResult<Option<String>> {
    let mut value = None;
    let mut rest = Vec::new();
    let mut index = 0;
    let equals_prefix = format!("{name}=");
    while index < args.len() {
        let arg = &args[index];
        if let Some(current) = arg.strip_prefix(&equals_prefix) {
            value = Some(current.to_string());
            index += 1;
        } else if arg == name {
            let current = args
                .get(index + 1)
                .ok_or_else(|| cli_error(format!("missing value for {name}")))?;
            value = Some(current.clone());
            index += 2;
        } else {
            rest.push(arg.clone());
            index += 1;
        }
    }
    *args = rest;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::take_session_mount_config;
    use std::path::Path;

    #[test]
    fn session_mount_extracts_persistent_cache_options() {
        let mut args = vec![
            "--mount".to_string(),
            "/home/mrvamp/Newsgroups".to_string(),
            "--mount-coherent-read-cache".to_string(),
            "--mount-read-cache".to_string(),
            "/var/cache/r9p/newsgroups".to_string(),
            "--mount-read-cache-max-bytes".to_string(),
            "1073741824".to_string(),
            "nucbox.mesh:9564".to_string(),
        ];
        let config = take_session_mount_config(&mut args).expect("mount config");

        assert!(config.coherent_read_cache);
        assert_eq!(
            config.read_cache_path.as_deref(),
            Some(Path::new("/var/cache/r9p/newsgroups"))
        );
        assert_eq!(config.read_cache_max_bytes, 1_073_741_824);
        assert_eq!(args, ["nucbox.mesh:9564"]);
    }
}
