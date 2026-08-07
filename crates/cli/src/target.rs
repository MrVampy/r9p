use std::{env, path::PathBuf, time::Duration};

use crate::errors::{cli_error, CliResult};
use crate::usage;

#[derive(Clone, Debug)]
pub(crate) struct Config {
    pub(crate) address: Option<String>,
    pub(crate) auth_config: Option<PathBuf>,
    pub(crate) auth_domain: Option<String>,
    pub(crate) aname: String,
    pub(crate) uname: String,
    pub(crate) msize: u32,
    pub(crate) msize_set: bool,
    pub(crate) machine: bool,
    pub(crate) request_timeout: Option<Duration>,
    pub(crate) control_timeout: Option<Duration>,
}

#[derive(Clone, Debug)]
pub(crate) struct Target {
    pub(crate) config: Config,
    pub(crate) path: String,
}

pub(crate) fn client_authentication(
    config: &Config,
) -> CliResult<session::ConnectionAuthentication> {
    let Some(path) = config.auth_config.clone() else {
        if config.auth_domain.is_some() {
            return Err(cli_error("--auth-domain requires --auth-config"));
        }
        return Ok(session::ConnectionAuthentication::Unauthenticated);
    };
    let credential =
        session::ClientCredential::new(path).map_err(|error| cli_error(error.to_string()))?;
    let authentication = match &config.auth_domain {
        Some(domain) => session::SessionAuthentication::authenticated_root(
            credential,
            session::ResponderName::new(domain).map_err(|error| cli_error(error.to_string()))?,
        ),
        None => session::SessionAuthentication::contained_root(credential),
    };
    Ok(session::ConnectionAuthentication::Session(authentication))
}

pub(crate) fn connection_target(config: Config, args: Vec<String>) -> CliResult<Target> {
    let path = match (config.address.is_some(), args.as_slice()) {
        (true, []) => "/".to_string(),
        (false, [path]) => path.clone(),
        _ => usage(),
    };
    Ok(Target { config, path })
}

pub(crate) fn target_path(target: &Target) -> CliResult<String> {
    match &target.config.address {
        Some(_) => Ok(target.path.clone()),
        None => {
            let (_, path) = split_namespace_path(&target.path)?;
            Ok(path)
        }
    }
}

pub(crate) fn split_namespace_path(path: &str) -> CliResult<(String, String)> {
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

pub(crate) fn namespace_socket(service: &str) -> CliResult<PathBuf> {
    let namespace = env::var("NAMESPACE")
        .map_err(|_| cli_error("NAMESPACE is required when -a is not provided"))?;
    Ok(PathBuf::from(namespace).join(service))
}

pub(crate) fn write_config_for_path(mut config: Config, path: &str) -> Config {
    if is_control_write_path(path) {
        config.request_timeout = config.control_timeout;
    }
    config
}

pub(crate) fn operation_config(mut config: Config) -> Config {
    config.request_timeout = config.control_timeout;
    config
}

fn is_control_write_path(path: &str) -> bool {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .is_some_and(|name| name == "ctl")
}
