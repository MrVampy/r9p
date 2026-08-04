use super::{
    connect_endpoint_with_timeouts, parse_tcp_address, path_names, Client, ConnectionTimeouts,
};
use crate::{
    codec,
    message::{RMessage, TMessage},
};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::{
    fs,
    os::unix::net::UnixListener,
    process,
    time::{SystemTime, UNIX_EPOCH},
};

#[test]
fn parses_plan9port_tcp_address() {
    let parsed = parse_tcp_address("tcp!127.0.0.1!19564").expect("address should parse");
    assert_eq!(parsed, "127.0.0.1:19564");
}

#[test]
fn defaults_bare_host_to_9p_port() {
    let parsed = parse_tcp_address("vault.local").expect("address should parse");
    assert_eq!(parsed, "vault.local:564");
}

#[test]
fn path_names_match_root_relative_walks() {
    assert_eq!(
        path_names("/entries/arch"),
        [b"entries".to_vec(), b"arch".to_vec()]
    );
    assert!(path_names("/").is_empty());
    assert!(path_names(".").is_empty());
}

#[test]
fn bounded_tcp_client_times_out_when_version_reply_stalls() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
    let address = listener
        .local_addr()
        .expect("listener should have an address");
    let client = thread::spawn(move || {
        let started = Instant::now();
        let result = Client::connect_tcp_with_timeouts(
            &address.to_string(),
            "test",
            "",
            codec::DEFAULT_MSIZE,
            short_test_timeouts(),
        );
        (result, started.elapsed())
    });

    let (mut stalled_stream, _) = listener.accept().expect("server should accept client");
    let request = codec::read_tmessage_checked(&mut stalled_stream, codec::MAX_MSIZE)
        .expect("version request should decode")
        .expect("client should send a version request");
    assert!(matches!(request, TMessage::Version { .. }));
    let (result, elapsed) = client.join().expect("client thread should finish");
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("stalled version reply should time out"),
    };

    assert!(error.display_lossy().contains("read 9P frame size"));
    assert!(elapsed < Duration::from_secs(1));
}

#[test]
fn bounded_tcp_client_times_out_when_attach_reply_stalls() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
    let address = listener
        .local_addr()
        .expect("listener should have an address");
    let client = thread::spawn(move || {
        let started = Instant::now();
        let result = Client::connect_tcp_with_timeouts(
            &address.to_string(),
            "test",
            "",
            codec::DEFAULT_MSIZE,
            short_test_timeouts(),
        );
        (result, started.elapsed())
    });

    let (mut stalled_stream, _) = listener.accept().expect("server should accept client");
    reply_to_version(&mut stalled_stream);
    let request = codec::read_tmessage_checked(&mut stalled_stream, codec::MAX_MSIZE)
        .expect("attach request should decode")
        .expect("client should send an attach request");
    assert!(matches!(request, TMessage::Attach { .. }));
    let (result, elapsed) = client.join().expect("client thread should finish");
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("stalled attach reply should time out"),
    };

    assert!(error.display_lossy().contains("read 9P frame size"));
    assert!(elapsed < Duration::from_secs(1));
}

#[test]
fn bounded_tcp_client_applies_distinct_transport_timeouts() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
    let address = listener
        .local_addr()
        .expect("listener should have an address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("server should accept client");
        reply_to_version(&mut stream);
        let request = codec::read_tmessage_checked(&mut stream, codec::MAX_MSIZE)
            .expect("attach request should decode")
            .expect("client should send an attach request");
        let tag = match request {
            TMessage::Attach { tag, .. } => tag,
            other => panic!("expected Tattach, got {other:?}"),
        };
        codec::write_rmessage_checked(
            &mut stream,
            codec::DEFAULT_MSIZE,
            &RMessage::Attach {
                tag,
                qid: crate::qid::Qid::dir(1),
            },
        )
        .expect("attach reply should encode");
    });
    let timeouts = ConnectionTimeouts::new(
        Duration::from_millis(80),
        Duration::from_millis(90),
        Duration::from_millis(100),
    );

    let client = Client::connect_tcp_with_timeouts(
        &address.to_string(),
        "test",
        "",
        codec::DEFAULT_MSIZE,
        timeouts,
    )
    .expect("bounded client should complete handshake");
    server.join().expect("server thread should finish");

    let applied_read = client.stream.read_timeout().expect("read timeout query");
    let applied_write = client.stream.write_timeout().expect("write timeout query");

    assert_socket_timeout_applied("read", applied_read, timeouts.read_timeout);
    assert_socket_timeout_applied("write", applied_write, timeouts.write_timeout);
    assert_ne!(
        applied_read, applied_write,
        "read and write timeouts should stay distinct after being applied"
    );
}

fn assert_socket_timeout_applied(which: &str, applied: Option<Duration>, requested: Duration) {
    const MAX_ROUNDING: Duration = Duration::from_millis(50);

    let applied = applied.unwrap_or_else(|| panic!("{which} timeout should be set"));
    assert!(
        applied >= requested && applied <= requested + MAX_ROUNDING,
        "{which} timeout should be the requested {requested:?} rounded up by at most \
         {MAX_ROUNDING:?}, got {applied:?}"
    );
}

#[test]
fn bounded_tcp_client_rejects_zero_timeouts() {
    let timeouts = ConnectionTimeouts::new(
        Duration::ZERO,
        Duration::from_secs(1),
        Duration::from_secs(1),
    );
    let result = Client::<TcpStream>::connect_tcp_with_timeouts(
        "127.0.0.1:9",
        "test",
        "",
        codec::DEFAULT_MSIZE,
        timeouts,
    );
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("zero timeout should be rejected before dialing"),
    };
    assert_eq!(
        error.display_lossy(),
        "connect timeout must be greater than zero"
    );
}

#[cfg(unix)]
#[test]
fn bounded_endpoint_client_connects_over_unix_socket() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should follow the Unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "r9p-blocking-endpoint-{}-{nonce}.sock",
        process::id()
    ));
    let listener = UnixListener::bind(&path).expect("Unix listener should bind");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("server should accept client");
        reply_to_version(&mut stream);
        let request = codec::read_tmessage_checked(&mut stream, codec::MAX_MSIZE)
            .expect("attach request should decode")
            .expect("client should send an attach request");
        let tag = match request {
            TMessage::Attach {
                tag, uname, aname, ..
            } => {
                assert_eq!(uname, b"service");
                assert_eq!(aname, b"/");
                tag
            }
            other => panic!("expected Tattach, got {other:?}"),
        };
        codec::write_rmessage_checked(
            &mut stream,
            codec::DEFAULT_MSIZE,
            &RMessage::Attach {
                tag,
                qid: crate::qid::Qid::dir(1),
            },
        )
        .expect("attach reply should encode");
    });
    let endpoint = format!("unix:{}", path.display());

    let client = connect_endpoint_with_timeouts(
        &endpoint,
        "service",
        "/",
        codec::DEFAULT_MSIZE,
        short_test_timeouts(),
    )
    .expect("Unix endpoint should complete the handshake");
    drop(client);
    server.join().expect("server thread should finish");
    fs::remove_file(path).expect("Unix socket should be removable");
}

fn short_test_timeouts() -> ConnectionTimeouts {
    ConnectionTimeouts::new(
        Duration::from_millis(100),
        Duration::from_millis(40),
        Duration::from_millis(100),
    )
}

fn reply_to_version(stream: &mut (impl Read + Write)) {
    let request = codec::read_tmessage_checked(stream, codec::MAX_MSIZE)
        .expect("version request should decode")
        .expect("client should send a version request");
    let (tag, msize) = match request {
        TMessage::Version { tag, msize, .. } => (tag, msize),
        other => panic!("expected Tversion, got {other:?}"),
    };
    codec::write_rmessage_checked(
        stream,
        msize,
        &RMessage::Version {
            tag,
            msize,
            version: b"9P2000".to_vec(),
        },
    )
    .expect("version reply should encode");
}
