use std::path::Path;
use std::time::Duration;

use super::{
    decode_mountinfo_path, mountinfo_targets_for_absolute, parse_mount_config,
    parse_mount_ensure_config, parse_mount_supervisor_config, systemd_command, SystemdUnitScope,
};
use crate::{target::Config, DEFAULT_MSIZE};

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
        request_timeout: Some(Duration::from_secs(30)),
        control_timeout: Some(Duration::from_secs(600)),
    }
}

#[test]
fn parses_final_mount_options() {
    let config = parse_mount_config(
        global(),
        vec![
            "--uname".to_string(),
            "glenda".to_string(),
            "--aname".to_string(),
            "/".to_string(),
            "--request-timeout".to_string(),
            "0.25".to_string(),
            "--connect-timeout".to_string(),
            "12".to_string(),
            "--lookup-timeout".to_string(),
            "0.5".to_string(),
            "--read-timeout".to_string(),
            "1".to_string(),
            "--write-timeout".to_string(),
            "2".to_string(),
            "--mutation-timeout".to_string(),
            "3".to_string(),
            "--control-timeout".to_string(),
            "4".to_string(),
            "--interrupt-timeout".to_string(),
            "0.125".to_string(),
            "--diagnostics-file".to_string(),
            "/tmp/r9p-mount-diagnostics.jsonl".to_string(),
            "--diagnostics-capacity".to_string(),
            "64".to_string(),
            "--status-file".to_string(),
            "/tmp/r9p-mount-status.json".to_string(),
            "--change-feed".to_string(),
            "/feeds/namespace".to_string(),
            "--change-feed-stream".to_string(),
            "/feeds/namespace/stream".to_string(),
            "--change-feed-cursor-template".to_string(),
            "/feeds/namespace-after/{event_id}".to_string(),
            "--change-feed-scope".to_string(),
            "session:mount-a".to_string(),
            "--change-feed-reconnect-delay".to_string(),
            "0.75".to_string(),
            "--change-feed-backpressure".to_string(),
            "128".to_string(),
            "--max-workers".to_string(),
            "8".to_string(),
            "--max-background".to_string(),
            "24".to_string(),
            "--congestion-threshold".to_string(),
            "18".to_string(),
            "--attr-timeout".to_string(),
            "1.5".to_string(),
            "--entry-timeout".to_string(),
            "2".to_string(),
            "--negative-timeout".to_string(),
            "5.25".to_string(),
            "--allow-other".to_string(),
            "--coherent-read-cache".to_string(),
            "--read-cache".to_string(),
            "/var/cache/r9p/newsgroups".to_string(),
            "--read-cache-max-bytes".to_string(),
            "1073741824".to_string(),
            "--source".to_string(),
            "/hosts/tuxedo/projects".to_string(),
            "--fallback-endpoint".to_string(),
            "nucbox.mesh:9564".to_string(),
            "--msize".to_string(),
            "8192".to_string(),
            "127.0.0.1:564".to_string(),
            "/tmp/r9p-mount".to_string(),
        ],
    )
    .expect("mount options should parse");

    assert_eq!(config.uname, "glenda");
    assert_eq!(config.aname, "/");
    assert_eq!(config.address, "127.0.0.1:564");
    assert_eq!(config.fallback_addresses, ["nucbox.mesh:9564"]);
    assert_eq!(config.source_path, "/hosts/tuxedo/projects");
    assert_eq!(config.mountpoint, "/tmp/r9p-mount");
    assert_eq!(config.request_timeout, Duration::from_millis(250));
    assert_eq!(config.lookup_timeout, Duration::from_millis(500));
    assert_eq!(config.read_timeout, Duration::from_secs(1));
    assert_eq!(config.write_timeout, Duration::from_secs(2));
    assert_eq!(config.mutation_timeout, Duration::from_secs(3));
    assert_eq!(config.control_timeout, Duration::from_secs(4));
    assert_eq!(config.interrupt_timeout, Duration::from_millis(125));
    assert_eq!(
        config.diagnostics_path.as_deref(),
        Some(std::path::Path::new("/tmp/r9p-mount-diagnostics.jsonl"))
    );
    assert_eq!(config.diagnostics_capacity, 64);
    assert_eq!(
        config.status_path.as_deref(),
        Some(std::path::Path::new("/tmp/r9p-mount-status.json"))
    );
    assert_eq!(config.change_feed_path.as_deref(), Some("/feeds/namespace"));
    assert_eq!(
        config.change_feed_stream_path.as_deref(),
        Some("/feeds/namespace/stream")
    );
    assert_eq!(
        config.change_feed_cursor_template.as_deref(),
        Some("/feeds/namespace-after/{event_id}")
    );
    assert_eq!(config.change_feed_scope.as_deref(), Some("session:mount-a"));
    assert_eq!(
        config.change_feed_reconnect_delay,
        Duration::from_millis(750)
    );
    assert_eq!(config.change_feed_backpressure_limit, 128);
    assert_eq!(config.attr_timeout, Duration::from_millis(1500));
    assert_eq!(config.entry_timeout, Duration::from_secs(2));
    assert_eq!(config.negative_timeout, Duration::from_millis(5250));
    assert!(config.allow_other);
    assert!(config.coherent_read_cache);
    assert_eq!(
        config.read_cache_path.as_deref(),
        Some(std::path::Path::new("/var/cache/r9p/newsgroups"))
    );
    assert_eq!(config.read_cache_max_bytes, 1_073_741_824);
    assert_eq!(config.max_workers, 8);
    assert_eq!(config.max_background, 24);
    assert_eq!(config.congestion_threshold, 18);
    assert_eq!(config.msize, 8192);
    assert_eq!(config.connect_timeout, Duration::from_secs(12));
}

#[test]
fn rejects_cursor_template_without_event_placeholder() {
    let result = parse_mount_config(
        global(),
        vec![
            "--change-feed".to_string(),
            "/feeds/namespace".to_string(),
            "--change-feed-cursor-template".to_string(),
            "/feeds/namespace-after/latest".to_string(),
            "127.0.0.1:564".to_string(),
            "/tmp/r9p-mount".to_string(),
        ],
    );

    assert!(result.is_err());
}

#[test]
fn mount_defaults_use_short_positive_kernel_cache() {
    let config = parse_mount_config(
        global(),
        vec!["127.0.0.1:564".to_string(), "/tmp/r9p-mount".to_string()],
    )
    .expect("mount options should parse");

    assert_eq!(config.attr_timeout, fuse::DEFAULT_ATTR_TIMEOUT);
    assert_eq!(config.entry_timeout, fuse::DEFAULT_ENTRY_TIMEOUT);
    assert_eq!(config.connect_timeout, Duration::from_secs(30));
    assert_eq!(config.source_path, "/");
    assert!(config.fallback_addresses.is_empty());
    assert!(!config.allow_other);
}

#[test]
fn mount_allows_explicit_zero_kernel_cache() {
    let config = parse_mount_config(
        global(),
        vec![
            "--attr-timeout".to_string(),
            "0".to_string(),
            "--entry-timeout".to_string(),
            "0".to_string(),
            "127.0.0.1:564".to_string(),
            "/tmp/r9p-mount".to_string(),
        ],
    )
    .expect("mount options should parse");

    assert_eq!(config.attr_timeout, Duration::ZERO);
    assert_eq!(config.entry_timeout, Duration::ZERO);
}

#[test]
fn derives_congestion_threshold_from_max_background() {
    let config = parse_mount_config(
        global(),
        vec![
            "--max-background".to_string(),
            "16".to_string(),
            "127.0.0.1:564".to_string(),
            "/tmp/r9p-mount".to_string(),
        ],
    )
    .expect("mount options should parse");

    assert_eq!(config.max_background, 16);
    assert_eq!(config.congestion_threshold, 12);
}

#[test]
fn rejects_unbounded_worker_and_queue_knobs() {
    for args in [
        vec![
            "--max-workers".to_string(),
            "0".to_string(),
            "127.0.0.1:564".to_string(),
            "/tmp/r9p-mount".to_string(),
        ],
        vec![
            "--max-background".to_string(),
            "2048".to_string(),
            "127.0.0.1:564".to_string(),
            "/tmp/r9p-mount".to_string(),
        ],
        vec![
            "--max-background".to_string(),
            "4".to_string(),
            "--congestion-threshold".to_string(),
            "8".to_string(),
            "127.0.0.1:564".to_string(),
            "/tmp/r9p-mount".to_string(),
        ],
        vec![
            "--change-feed-backpressure".to_string(),
            "0".to_string(),
            "127.0.0.1:564".to_string(),
            "/tmp/r9p-mount".to_string(),
        ],
    ] {
        assert!(parse_mount_config(global(), args).is_err());
    }
}

#[test]
fn rejects_old_mount_short_options() {
    for option in ["-a", "-E", "-T"] {
        let result = parse_mount_config(
            global(),
            vec![
                option.to_string(),
                "1".to_string(),
                "127.0.0.1:564".to_string(),
                "/tmp/r9p-mount".to_string(),
            ],
        );
        assert!(result.is_err(), "{option} should not parse");
    }
}

#[test]
fn dash_upper_a_is_aname_not_attr_timeout() {
    let config = parse_mount_config(
        global(),
        vec![
            "-A".to_string(),
            "/".to_string(),
            "127.0.0.1:564".to_string(),
            "/tmp/r9p-mount".to_string(),
        ],
    )
    .expect("mount options should parse");

    assert_eq!(config.aname, "/");
    assert_eq!(config.attr_timeout, fuse::DEFAULT_ATTR_TIMEOUT);
}

#[test]
fn mount_rejects_global_address_option() {
    let mut global = global();
    global.address = Some("127.0.0.1:564".to_string());
    let result = parse_mount_config(global, vec!["/tmp/r9p-mount".to_string()]);

    assert!(result.is_err());
}

#[test]
fn parses_mount_supervisor_options() {
    let config = parse_mount_supervisor_config(vec![
        "--mountpoint".to_string(),
        ".vault/live".to_string(),
        "--unit".to_string(),
        "vault-runtime-r9p-live-mount".to_string(),
        "--unit-scope".to_string(),
        "user".to_string(),
        "--expect-endpoint".to_string(),
        "192.168.0.30:9564".to_string(),
        "--expect-change-feed".to_string(),
        "/feeds/namespace".to_string(),
        "--expect-status-file".to_string(),
        ".vault/live.status.json".to_string(),
        "--status-file".to_string(),
        ".vault/live.status.json".to_string(),
        "--attempts".to_string(),
        "3".to_string(),
    ])
    .expect("supervisor options should parse");
    let cwd = std::env::current_dir().expect("current dir");

    assert_eq!(config.mountpoint, cwd.join(".vault/live"));
    assert_eq!(config.unit.as_deref(), Some("vault-runtime-r9p-live-mount"));
    assert_eq!(config.unit_scope, Some(SystemdUnitScope::User));
    assert_eq!(
        config.expected_endpoint.as_deref(),
        Some("192.168.0.30:9564")
    );
    assert_eq!(
        config.expected_change_feed.as_deref(),
        Some("/feeds/namespace")
    );
    assert_eq!(
        config.expected_status_file.as_deref(),
        Some(".vault/live.status.json")
    );
    assert_eq!(
        config.status_file.as_deref(),
        Some(Path::new(".vault/live.status.json"))
    );
    assert_eq!(config.attempts, 3);
}

#[test]
fn mount_supervisor_requires_a_scope_for_a_unit() {
    let error = parse_mount_supervisor_config(vec![
        "--mountpoint".to_string(),
        ".vault/live".to_string(),
        "--unit".to_string(),
        "vault-live-r9p-mount.service".to_string(),
    ])
    .expect_err("a unit without its manager scope must fail");

    assert_eq!(
        error.to_string(),
        "--unit requires --unit-scope user|system"
    );
}

#[test]
fn mount_supervisor_parses_a_system_unit_scope() {
    let config = parse_mount_supervisor_config(vec![
        "--mountpoint".to_string(),
        ".vault/live".to_string(),
        "--unit".to_string(),
        "vault-live-r9p-mount.service".to_string(),
        "--unit-scope".to_string(),
        "system".to_string(),
    ])
    .expect("a system unit scope should parse");

    assert_eq!(config.unit_scope, Some(SystemdUnitScope::System));
}

#[test]
fn systemd_commands_target_the_selected_manager() {
    let user = systemd_command("systemctl", SystemdUnitScope::User);
    let system = systemd_command("systemctl", SystemdUnitScope::System);

    assert_eq!(user.get_args().collect::<Vec<_>>(), vec!["--user"]);
    assert!(system.get_args().next().is_none());
}

#[test]
fn parses_mount_ensure_options_and_mount_invocation() {
    let (config, mount_args) = parse_mount_ensure_config(vec![
        "--mountpoint".to_string(),
        ".vault/live".to_string(),
        "--unit".to_string(),
        "vault-runtime-r9p-live-mount".to_string(),
        "--unit-scope".to_string(),
        "user".to_string(),
        "--attempts".to_string(),
        "4".to_string(),
        "--".to_string(),
        "--uname".to_string(),
        "codex".to_string(),
        "192.168.0.30:9564".to_string(),
        ".vault/live".to_string(),
    ])
    .expect("ensure options should parse");
    let cwd = std::env::current_dir().expect("current dir");

    assert_eq!(config.mountpoint, cwd.join(".vault/live"));
    assert_eq!(config.unit.as_deref(), Some("vault-runtime-r9p-live-mount"));
    assert_eq!(config.unit_scope, Some(SystemdUnitScope::User));
    assert_eq!(config.attempts, 4);
    assert_eq!(
        mount_args,
        vec![
            "--uname".to_string(),
            "codex".to_string(),
            "192.168.0.30:9564".to_string(),
            ".vault/live".to_string()
        ]
    );
}

#[test]
fn parses_mountinfo_targets_for_absolute_mountpoint() {
    let mountinfo = concat!(
        "42 28 0:37 / /sys/fs/fuse/connections rw - fusectl fusectl rw\n",
        "68 30 0:57 / /home/mrvamp/Vault/.vault/live rw - fuse /dev/fuse rw,user_id=1000\n",
        "69 30 0:58 / /home/mrvamp/Vault/.vault/live rw - fuse /dev/fuse rw,user_id=1000\n",
    );

    assert_eq!(
        mountinfo_targets_for_absolute(mountinfo, "/home/mrvamp/Vault/.vault/live"),
        vec![
            "/home/mrvamp/Vault/.vault/live".to_string(),
            "/home/mrvamp/Vault/.vault/live".to_string()
        ]
    );
}

#[test]
fn decodes_mountinfo_octal_escapes() {
    assert_eq!(
        "/tmp/r9p mount/live",
        decode_mountinfo_path("/tmp/r9p\\040mount/live")
    );
}
