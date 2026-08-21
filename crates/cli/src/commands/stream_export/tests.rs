use std::path::PathBuf;

use crate::{target::Config, DEFAULT_MSIZE};

use super::config::parse;

fn global() -> Config {
    Config {
        auth_domain: None,
        address: None,
        auth_config: None,
        aname: String::new(),
        uname: "standby".to_string(),
        msize: DEFAULT_MSIZE,
        msize_set: false,
        machine: false,
        request_timeout: Some(std::time::Duration::from_secs(30)),
        control_timeout: Some(std::time::Duration::from_secs(600)),
    }
}

#[test]
fn parses_a_fixed_absolute_command_and_exact_principals() {
    let config = parse(
        global(),
        vec![
            "--bind".to_string(),
            "127.0.0.1:9568".to_string(),
            "--auth-config".to_string(),
            "/run/keys/server.conf".to_string(),
            "--allow-principal".to_string(),
            "/srv/coordinator/nucbox".to_string(),
            "--status-file".to_string(),
            "/run/stream-export/status".to_string(),
            "--".to_string(),
            "/nix/store/git/bin/git".to_string(),
            "daemon".to_string(),
            "--inetd".to_string(),
        ],
    )
    .expect("stream export config");

    assert_eq!(config.bind, "127.0.0.1:9568".parse().expect("bind"));
    assert_eq!(config.auth_config, PathBuf::from("/run/keys/server.conf"));
    assert!(config
        .allowed_principals
        .contains("/srv/coordinator/nucbox"));
    assert_eq!(
        config.command.program,
        PathBuf::from("/nix/store/git/bin/git")
    );
    assert_eq!(config.command.arguments, ["daemon", "--inetd"]);
    assert_eq!(
        config.status_file,
        Some(PathBuf::from("/run/stream-export/status"))
    );
}

#[test]
fn rejects_ambient_commands_and_unbounded_resource_settings() {
    for arguments in [
        vec![
            "--bind",
            "127.0.0.1:9568",
            "--auth-config",
            "/run/keys/server.conf",
            "--allow-principal",
            "standby",
            "--",
            "git",
        ],
        vec![
            "--bind",
            "127.0.0.1:9568",
            "--auth-config",
            "/run/keys/server.conf",
            "--allow-principal",
            "standby",
            "--max-sessions",
            "0",
            "--",
            "/bin/true",
        ],
        vec![
            "--bind",
            "127.0.0.1:9568",
            "--auth-config",
            "/run/keys/server.conf",
            "--allow-principal",
            "standby",
            "--max-buffer-bytes",
            "1",
            "--",
            "/bin/true",
        ],
    ] {
        assert!(parse(
            global(),
            arguments.into_iter().map(str::to_string).collect()
        )
        .is_err());
    }
}

#[test]
fn requires_an_authenticated_principal_allowlist() {
    let result = parse(
        global(),
        vec![
            "--bind".to_string(),
            "127.0.0.1:9568".to_string(),
            "--auth-config".to_string(),
            "/run/keys/server.conf".to_string(),
            "--".to_string(),
            "/bin/true".to_string(),
        ],
    );
    assert!(result.is_err());
}
