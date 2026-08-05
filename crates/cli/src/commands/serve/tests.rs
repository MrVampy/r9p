use super::{parse_export_config, parse_serve_config, required_nofile_limit, BindTarget};
use crate::{target::Config, DEFAULT_MSIZE};
use std::{
    fs,
    net::SocketAddr,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_AUTH_CONFIG: AtomicU64 = AtomicU64::new(1);

fn global() -> Config {
    Config {
        auth_domain: None,
        address: None,
        auth_config: None,
        aname: String::new(),
        uname: "codex".to_string(),
        msize: DEFAULT_MSIZE,
        msize_set: false,
        machine: false,
        request_timeout: Some(std::time::Duration::from_secs(30)),
        control_timeout: Some(std::time::Duration::from_secs(600)),
    }
}

fn server_auth_config() -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "r9p-export-auth-test-{}-{}",
        std::process::id(),
        NEXT_AUTH_CONFIG.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("auth test directory");
    let server = r9p_auth::generate_key_pair().expect("server key pair");
    let client = r9p_auth::generate_key_pair().expect("client key pair");
    let private = root.join("server.key");
    let public = root.join("server.pub");
    r9p_auth::write_key_pair(&private, &public, &server).expect("write server key pair");
    let config = root.join("server.conf");
    fs::write(
        &config,
        format!(
            "format r9p-session-auth.v1\nrole server\ndomain vault\nprivate-key {}\npeer {} codex\n",
            private.display(),
            client.public
        ),
    )
    .expect("write server auth config");
    (root, config)
}

#[test]
fn parses_loopback_tcp_bind() {
    let config = parse_serve_config(
        global(),
        vec![
            "--bind".to_string(),
            "127.0.0.1:0".to_string(),
            "/tmp/export".to_string(),
        ],
    )
    .expect("serve config should parse");
    assert_eq!(
        config.bind,
        BindTarget::Tcp("127.0.0.1:0".parse::<SocketAddr>().expect("socket address"))
    );
    assert_eq!(config.root, PathBuf::from("/tmp/export"));
    assert!(!config.writable);
}

#[test]
fn nofile_budget_includes_fid_margin() {
    assert_eq!(4352, required_nofile_limit(4096).expect("limit"));
}

#[test]
fn parses_plan9_tcp_bind() {
    let config = parse_serve_config(
        global(),
        vec![
            "--bind".to_string(),
            "tcp!127.0.0.1!0".to_string(),
            "/tmp/export".to_string(),
        ],
    )
    .expect("serve config should parse");
    assert_eq!(
        config.bind,
        BindTarget::Tcp("127.0.0.1:0".parse::<SocketAddr>().expect("socket address"))
    );
}

#[test]
fn parses_unix_bind() {
    let config = parse_serve_config(
        global(),
        vec![
            "--bind".to_string(),
            "unix:/tmp/r9p.sock".to_string(),
            "/tmp/export".to_string(),
        ],
    )
    .expect("serve config should parse");
    assert_eq!(
        config.bind,
        BindTarget::Unix(PathBuf::from("/tmp/r9p.sock"))
    );
}

#[test]
fn rejects_non_loopback_tcp_bind_without_auth_boundary() {
    let result = parse_serve_config(
        global(),
        vec![
            "--bind".to_string(),
            "192.0.2.10:564".to_string(),
            "/tmp/export".to_string(),
        ],
    );
    assert!(result.is_err());
}

#[test]
fn parses_non_loopback_export_bind_with_enforced_session_auth() {
    let (root, auth_config) = server_auth_config();
    let config = parse_export_config(
        global(),
        vec![
            "--bind".to_string(),
            "192.0.2.10:564".to_string(),
            "--auth-config".to_string(),
            auth_config.to_string_lossy().into_owned(),
            "/tmp/export".to_string(),
        ],
    )
    .expect("export config should parse");
    assert_eq!(
        config.serve.bind,
        BindTarget::Tcp(
            "192.0.2.10:564"
                .parse::<SocketAddr>()
                .expect("socket address")
        )
    );
    assert_eq!(config.auth.render(), "p9any:noise-ik@vault");
    fs::remove_dir_all(root).expect("remove auth test directory");
}

#[test]
fn rejects_non_loopback_export_bind_without_auth_boundary() {
    let result = parse_export_config(
        global(),
        vec![
            "--bind".to_string(),
            "192.0.2.10:564".to_string(),
            "/tmp/export".to_string(),
        ],
    );
    assert!(result.is_err());
}

#[test]
fn parses_unix_export_descriptor_file_without_network_auth() {
    let config = parse_export_config(
        global(),
        vec![
            "--bind".to_string(),
            "unix:/tmp/r9p.sock".to_string(),
            "--descriptor-file".to_string(),
            "/tmp/r9p.desc".to_string(),
            "/tmp/export".to_string(),
        ],
    )
    .expect("export config should parse");
    assert_eq!(config.descriptor_file, Some(PathBuf::from("/tmp/r9p.desc")));
    assert_eq!(config.auth.render(), "none");
}

#[test]
fn rejects_session_auth_on_unix_export() {
    let (root, auth_config) = server_auth_config();
    let result = parse_export_config(
        global(),
        vec![
            "--bind".to_string(),
            "unix:/tmp/r9p.sock".to_string(),
            "--auth-config".to_string(),
            auth_config.to_string_lossy().into_owned(),
            "/tmp/export".to_string(),
        ],
    );
    assert!(result.is_err());
    fs::remove_dir_all(root).expect("remove auth test directory");
}

#[test]
fn parses_export_descriptor_extension_fields() {
    let config = parse_export_config(
        global(),
        vec![
            "--bind".to_string(),
            "127.0.0.1:0".to_string(),
            "--descriptor-field".to_string(),
            "content_path=/export/content.bin".to_string(),
            "/tmp/export".to_string(),
        ],
    )
    .expect("export config should parse");
    assert_eq!(
        config.extra_fields.get("content_path").map(String::as_str),
        Some("/export/content.bin")
    );
}

#[test]
fn parses_writable_export_mode() {
    let config = parse_export_config(
        global(),
        vec!["--writable".to_string(), "/tmp/export".to_string()],
    )
    .expect("export config should parse");
    assert!(config.serve.writable);
}
