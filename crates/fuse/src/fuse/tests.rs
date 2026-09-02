use super::config::DEFAULT_CHANGE_FEED_RECONNECT_DELAY;
use super::dispatch::{
    fuse_init_out, max_request_pages, source_binding_retryable, supported_init_flags,
    wait_for_managed_input, ManagedInput,
};
use super::mount_state::negative_entry_out;
use super::ops::encode_dirents;
use super::util::{
    dirent_size, flags_to_9p_mode, fuse_name_offset, fuse_open_flags,
    is_lookup_namespace_shape_error, is_namespace_shape_error, is_transport_error,
};
use super::wire::{
    FuseInitIn, DEFAULT_MAX_IO_BYTES, FOPEN_DIRECT_IO, FOPEN_KEEP_CACHE, FUSE_KERNEL_VERSION,
    FUSE_MAX_PAGES,
};
use super::{
    change_feed, default_congestion_threshold, normalize_config, parse_source_path,
    read_cache_volume_identity, Config, DEFAULT_MAX_BACKGROUND, DEFAULT_MAX_WORKERS,
};
use crate::error::Error;
use crate::node::DirEntry;
use r9p::{qid::Qid, stat::Stat};
use session::{ORDWR, OREAD, OTRUNC, OWRITE};
use std::{
    io::Write, net::Shutdown, os::fd::AsRawFd, os::unix::net::UnixStream, sync::mpsc, thread,
    time::Duration,
};

#[test]
fn managed_mount_wait_wakes_on_private_shutdown_descriptor() {
    let (fuse_reader, _fuse_writer) = UnixStream::pair().expect("create fake FUSE descriptor");
    let (shutdown_reader, shutdown_writer) =
        UnixStream::pair().expect("create shutdown descriptor");
    let (completed, observed) = mpsc::channel();
    let waiter = thread::spawn(move || {
        let result = wait_for_managed_input(fuse_reader.as_raw_fd(), shutdown_reader.as_raw_fd());
        completed.send(result).expect("report managed wait result");
    });

    shutdown_writer
        .shutdown(Shutdown::Both)
        .expect("signal managed shutdown");
    assert_eq!(
        observed
            .recv_timeout(Duration::from_secs(2))
            .expect("managed wait should stop without FUSE input")
            .expect("managed wait should succeed"),
        ManagedInput::Shutdown
    );
    waiter.join().expect("managed wait thread");
}

#[test]
fn managed_mount_wait_preserves_fuse_readiness() {
    let (fuse_reader, mut fuse_writer) = UnixStream::pair().expect("create fake FUSE descriptor");
    let (shutdown_reader, _shutdown_writer) =
        UnixStream::pair().expect("create shutdown descriptor");
    let (completed, observed) = mpsc::channel();
    let waiter = thread::spawn(move || {
        let result = wait_for_managed_input(fuse_reader.as_raw_fd(), shutdown_reader.as_raw_fd());
        completed.send(result).expect("report managed wait result");
    });

    fuse_writer.write_all(&[1]).expect("signal FUSE readiness");
    assert_eq!(
        observed
            .recv_timeout(Duration::from_secs(2))
            .expect("managed wait should observe FUSE input")
            .expect("managed wait should succeed"),
        ManagedInput::Fuse
    );
    waiter.join().expect("managed wait thread");
}

#[test]
fn maps_truncating_write_flags_to_9p_mode() {
    let flags = libc::O_WRONLY as u32 | libc::O_TRUNC as u32;
    assert_eq!(flags_to_9p_mode(flags), OWRITE | OTRUNC);
}

#[test]
fn maps_read_only_flags_to_9p_read() {
    assert_eq!(flags_to_9p_mode(libc::O_RDONLY as u32), OREAD);
}

#[test]
fn directory_encoding_respects_buffer_size() {
    let entry = DirEntry {
        name: b"alpha".to_vec(),
        qid: Qid::file(7),
        stat: Stat::new("alpha", Qid::file(7), 0o444),
    };
    let bytes = encode_dirents(100, 200, 0, 1024, &[entry]).expect("dirents should encode");
    assert!(!bytes.is_empty());
    let too_small = encode_dirents(100, 200, 0, 1, &[]).expect("dirents should encode");
    assert!(too_small.is_empty());
}

#[test]
fn directory_encoding_matches_linux_fuse_dirent_parser() {
    let entries = vec![
        DirEntry {
            name: b"active_artifact_loaded_drift".to_vec(),
            qid: Qid::file(7),
            stat: Stat::new("active_artifact_loaded_drift", Qid::file(7), 0o444),
        },
        DirEntry {
            name: b"active_artifact_loaded_drift_summary".to_vec(),
            qid: Qid::file(8),
            stat: Stat::new("active_artifact_loaded_drift_summary", Qid::file(8), 0o444),
        },
        DirEntry {
            name: b"old_code_module_count".to_vec(),
            qid: Qid::file(9),
            stat: Stat::new("old_code_module_count", Qid::file(9), 0o444),
        },
    ];

    let bytes = encode_dirents(100, 200, 0, 1024, &entries).expect("dirents should encode");

    assert_eq!(
        linux_parse_dirent_names(&bytes),
        vec![
            ".".to_string(),
            "..".to_string(),
            "active_artifact_loaded_drift".to_string(),
            "active_artifact_loaded_drift_summary".to_string(),
            "old_code_module_count".to_string(),
        ]
    );
}

#[test]
fn directory_encoding_uses_supplied_special_entry_inodes() {
    let bytes = encode_dirents(100, 200, 0, 1024, &[]).expect("dirents should encode");
    let first_ino = u64::from_le_bytes(bytes[0..8].try_into().expect("first ino"));
    let second_offset = dirent_size(1);
    let second_ino = u64::from_le_bytes(
        bytes[second_offset..second_offset + 8]
            .try_into()
            .expect("second ino"),
    );

    assert_eq!(first_ino, 100);
    assert_eq!(second_ino, 200);
}

#[test]
fn dirent_size_uses_linux_name_offset_not_rust_flexible_array_size() {
    assert_eq!(fuse_name_offset(), 24);
    assert_eq!(dirent_size(1), 32);
    assert_eq!(dirent_size(2), 32);
    assert_eq!(dirent_size(9), 40);
}

#[test]
fn read_capable_file_opens_use_direct_io_for_unknown_size_reads() {
    assert_eq!(fuse_open_flags(false, OREAD, 0, true), FOPEN_DIRECT_IO);
    assert_eq!(fuse_open_flags(false, ORDWR, 20, true), FOPEN_DIRECT_IO);
    assert_eq!(
        fuse_open_flags(false, ORDWR | OTRUNC, 20, true),
        FOPEN_DIRECT_IO
    );
    assert_eq!(fuse_open_flags(false, OWRITE, 20, true), 0);
    assert_eq!(fuse_open_flags(false, OWRITE | OTRUNC, 20, true), 0);
}

#[test]
fn coherent_known_length_reads_reuse_the_kernel_page_cache() {
    assert_eq!(fuse_open_flags(false, OREAD, 20, true), FOPEN_KEEP_CACHE);
    assert_eq!(fuse_open_flags(false, OREAD, 20, false), FOPEN_DIRECT_IO);
}

#[test]
fn directory_opens_keep_readdir_demand_visible() {
    assert_eq!(fuse_open_flags(true, OREAD, 0, false), 0);
}

#[test]
fn shape_recovery_forces_reconnect_after_threshold_with_cooldown() {
    use std::time::{Duration, Instant};
    let mut recovery = super::ShapeRecovery::new();
    let start = Instant::now();
    for i in 0..7 {
        assert!(!recovery.note_at(start + Duration::from_millis(i)));
    }
    assert!(recovery.note_at(start + Duration::from_millis(7)));
    for i in 8..16 {
        assert!(!recovery.note_at(start + Duration::from_millis(i)));
    }
    let later = start + Duration::from_secs(6);
    assert!(recovery.note_at(later));
    assert!(!recovery.note_at(later + Duration::from_millis(1)));
    let mut spaced = super::ShapeRecovery::new();
    for i in 0..20 {
        assert!(!spaced.note_at(start + Duration::from_secs(11 * i)));
    }
}

#[test]
fn namespace_shape_errors_are_reconnect_candidates() {
    assert!(is_namespace_shape_error(&Error::new(
        libc::ENOENT,
        "walk failed after namespace refresh",
    )));
    assert!(is_namespace_shape_error(&Error::new(
        libc::ESTALE,
        "unknown fid",
    )));
    assert!(is_namespace_shape_error(&Error::new(
        libc::EBADF,
        "unknown namespace fid 6",
    )));
    assert!(!is_namespace_shape_error(&Error::new(
        libc::EACCES,
        "permission denied",
    )));
    assert!(!is_namespace_shape_error(&Error::new(
        libc::ESTALE,
        "application-level stale value",
    )));
    assert!(!is_lookup_namespace_shape_error(&Error::new(
        libc::ENOENT,
        "partial walk",
    )));
    assert!(is_lookup_namespace_shape_error(&Error::new(
        libc::EBADF,
        "unknown namespace fid 6",
    )));
}

#[test]
fn negative_lookup_reply_has_zero_nodeid_and_configured_validity() {
    let out = negative_entry_out(Duration::from_millis(5250));

    assert_eq!(out.nodeid, 0);
    assert_eq!(out.generation, 0);
    assert_eq!(out.entry_valid, 5);
    assert_eq!(out.entry_valid_nsec, 250_000_000);
    assert_eq!(out.attr_valid, 0);
    assert_eq!(out.attr_valid_nsec, 0);
}

#[test]
fn closed_9p_reader_errors_are_reconnect_candidates() {
    assert!(is_transport_error(&Error::new(
        libc::ENOTCONN,
        "9P client state: 9P reader stopped before response",
    )));
    assert!(is_transport_error(&Error::new(
        libc::EIO,
        "9P client state: 9P reader stopped before response",
    )));
    assert!(!is_transport_error(&Error::new(
        libc::EPROTO,
        "9P client state: response tag mismatch",
    )));
}

#[test]
fn reconnect_waits_only_for_transient_source_publication_failures() {
    for errno in [
        libc::ENOENT,
        libc::ESTALE,
        libc::EAGAIN,
        libc::ETIMEDOUT,
        libc::ENOTCONN,
        libc::ECONNREFUSED,
        libc::ECONNRESET,
        libc::ECONNABORTED,
        libc::EPIPE,
        libc::ENETDOWN,
        libc::ENETUNREACH,
        libc::EHOSTDOWN,
        libc::EHOSTUNREACH,
    ] {
        assert!(source_binding_retryable(&Error::new(errno, "transient")));
    }
    for errno in [libc::EACCES, libc::EPERM, libc::EPROTO, libc::EINVAL] {
        assert!(!source_binding_retryable(&Error::new(errno, "fatal")));
    }
}

#[test]
fn default_congestion_threshold_matches_kernel_ratio() {
    assert_eq!(default_congestion_threshold(12), 9);
    assert_eq!(default_congestion_threshold(1), 1);
}

#[test]
fn mount_source_path_is_absolute_and_canonical() {
    assert_eq!(
        parse_source_path("/hosts/tuxedo/projects").expect("canonical source path"),
        vec![b"hosts".to_vec(), b"tuxedo".to_vec(), b"projects".to_vec()]
    );
    assert!(parse_source_path("/").expect("root source path").is_empty());
    for invalid in [
        "hosts/tuxedo",
        "",
        "/hosts//tuxedo",
        "/hosts/./tuxedo",
        "/hosts/../tuxedo",
        "/hosts/tuxedo/",
    ] {
        assert!(
            parse_source_path(invalid).is_err(),
            "{invalid:?} should be rejected"
        );
    }
}

#[test]
fn init_does_not_claim_exportfs_stale_handle_support() {
    const FUSE_EXPORT_SUPPORT_BIT: u32 = 1 << 4;

    assert_eq!(supported_init_flags() & FUSE_EXPORT_SUPPORT_BIT, 0);
}

#[test]
fn init_leaves_caller_umask_application_to_linux() {
    const FUSE_DONT_MASK_BIT: u32 = 1 << 6;

    assert_eq!(supported_init_flags() & FUSE_DONT_MASK_BIT, 0);
}

#[test]
fn init_admits_requests_larger_than_the_linux_default_page_count() {
    assert_ne!(supported_init_flags() & FUSE_MAX_PAGES, 0);
    assert_eq!(
        max_request_pages(DEFAULT_MAX_IO_BYTES - 23, 4096).expect("4 KiB pages"),
        255
    );
    assert_eq!(
        max_request_pages(DEFAULT_MAX_IO_BYTES - 23, 65_536).expect("64 KiB pages"),
        15
    );
    let output = fuse_init_out(
        FuseInitIn {
            major: FUSE_KERNEL_VERSION,
            minor: 31,
            max_readahead: DEFAULT_MAX_IO_BYTES,
            flags: FUSE_MAX_PAGES,
        },
        64,
        48,
        4096,
        DEFAULT_MAX_IO_BYTES - 23,
    )
    .expect("FUSE init output");
    assert_ne!(output.flags & FUSE_MAX_PAGES, 0);
    assert_eq!(output.max_write, 255 * 4096);
    assert_eq!(output.max_pages, 255);
}

#[test]
fn mount_config_normalization_keeps_worker_and_background_limits_nonzero() {
    let mut config = Config {
        address: "127.0.0.1:564".to_string(),
        fallback_addresses: Vec::new(),
        authentication: session::ConnectionAuthentication::Unauthenticated,
        source_path: "/".to_string(),
        mountpoint: "/tmp/r9p-mount".to_string(),
        uname: "codex".to_string(),
        aname: "/".to_string(),
        msize: 8192,
        connect_timeout: Duration::from_secs(30),
        attr_timeout: Duration::ZERO,
        entry_timeout: Duration::ZERO,
        negative_timeout: Duration::ZERO,
        request_timeout: Duration::from_secs(5),
        lookup_timeout: Duration::ZERO,
        read_timeout: Duration::ZERO,
        change_feed_read_timeout: Duration::ZERO,
        write_timeout: Duration::ZERO,
        mutation_timeout: Duration::ZERO,
        control_timeout: Duration::ZERO,
        interrupt_timeout: Duration::ZERO,
        max_workers: 0,
        max_background: 0,
        congestion_threshold: 99,
        diagnostics_path: None,
        diagnostics_capacity: 0,
        status_path: None,
        change_feed_path: None,
        change_feed_stream_path: None,
        change_feed_cursor_template: None,
        change_feed_scope: None,
        change_feed_reconnect_delay: Duration::ZERO,
        change_feed_backpressure_limit: 0,
        coherent_read_cache: false,
        read_cache_path: None,
        read_cache_max_bytes: 0,
        allow_other: false,
        debug: false,
    };

    normalize_config(&mut config).expect("normalize config");

    assert_eq!(config.lookup_timeout, Duration::from_secs(5));
    assert_eq!(config.change_feed_read_timeout, Duration::from_secs(5));
    assert_eq!(config.interrupt_timeout, Duration::from_secs(1));
    assert_eq!(config.max_workers, DEFAULT_MAX_WORKERS);
    assert_eq!(
        config.change_feed_reconnect_delay,
        DEFAULT_CHANGE_FEED_RECONNECT_DELAY
    );
    assert_eq!(
        config.change_feed_backpressure_limit,
        change_feed::DEFAULT_CHANGE_FEED_BACKPRESSURE_LIMIT
    );
    assert_eq!(config.max_background, DEFAULT_MAX_BACKGROUND);
    assert_eq!(
        config.congestion_threshold,
        default_congestion_threshold(DEFAULT_MAX_BACKGROUND)
    );

    let mut persistent = config.clone();
    persistent.coherent_read_cache = true;
    persistent.read_cache_path = Some("/var/cache/r9p/newsgroups".into());
    persistent.read_cache_max_bytes = 4 * 1024 * 1024;
    normalize_config(&mut persistent).expect("complete persistent cache config");
    assert_eq!(
        read_cache_volume_identity(&persistent)
            .expect_err("unauthenticated cache identity must fail")
            .errno,
        libc::EINVAL
    );
    persistent.authentication = session::SessionAuthentication::authenticated_root(
        session::ClientCredential::new("/tmp/r9p-test-client.conf").expect("credential"),
        session::ResponderName::new("coordinator").expect("responder"),
    )
    .into();
    let identity = read_cache_volume_identity(&persistent).expect("cache identity");
    persistent.source_path = "/sources/newsgroups/downloads/files".to_string();
    assert_ne!(
        read_cache_volume_identity(&persistent).expect("source-bound cache identity"),
        identity
    );

    persistent.read_cache_path = Some("relative/cache".into());
    assert_eq!(
        normalize_config(&mut persistent)
            .expect_err("relative persistent cache path must fail")
            .errno,
        libc::EINVAL
    );

    let mut incomplete = config.clone();
    incomplete.read_cache_max_bytes = 4 * 1024 * 1024;
    assert_eq!(
        normalize_config(&mut incomplete)
            .expect_err("cache quota without cache path must fail")
            .errno,
        libc::EINVAL
    );

    config.change_feed_path = Some("/events/namespace/recent".to_string());
    assert_eq!(
        normalize_config(&mut config)
            .expect_err("change feed without blocking stream must fail")
            .errno,
        libc::EINVAL
    );
}

fn linux_parse_dirent_names(bytes: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    let mut offset = 0_usize;
    while bytes.len().saturating_sub(offset) >= fuse_name_offset() {
        let namelen_offset = offset + 16;
        let namelen = u32::from_ne_bytes(
            bytes[namelen_offset..namelen_offset + 4]
                .try_into()
                .expect("namelen"),
        ) as usize;
        assert!(namelen > 0, "linux FUSE rejects zero-length names");
        let name_start = offset + fuse_name_offset();
        let name_end = name_start + namelen;
        assert!(name_end <= bytes.len(), "name overruns reply");
        let name = &bytes[name_start..name_end];
        assert!(!name.contains(&b'/'), "linux FUSE rejects slash in names");
        names.push(String::from_utf8(name.to_vec()).expect("utf8 name"));
        offset += dirent_size(namelen);
    }
    assert_eq!(
        offset,
        bytes.len(),
        "reply must not contain trailing records"
    );
    names
}
