use front::abi::{
    r9p_front_abi_version, r9p_front_append_event, r9p_front_complete_request,
    r9p_front_complete_write, r9p_front_free, r9p_front_last_error, r9p_front_maintain_r9p_export,
    r9p_front_new, r9p_front_next_request, r9p_front_publish_r9p_export,
    r9p_front_reconcile_r9p_exports, r9p_front_register_intake, r9p_front_register_log,
    r9p_front_register_rpc, r9p_front_register_write_relay, r9p_front_request_context_copy,
    r9p_front_request_copy, r9p_front_request_prefix_copy, r9p_front_serve_tcp, r9p_front_set,
    r9p_front_set_principal_class_aname, r9p_front_set_principal_root,
    r9p_front_set_protocol_limits, r9p_front_set_pushed_directory, r9p_front_set_pushed_file,
    r9p_front_stop,
};
use front::Front;
use r9p::blocking::{Client, OWRITE};
use r9p::fid::NOFID;
use r9p::message::{RMessage, TMessage, NOTAG};
use r9p::qid::DMDIR;
use r9p::stat::decode_dir_entries;
use r9p::{codec, Error};
use std::ffi::c_char;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::thread;
use std::time::{Duration, Instant};

fn cstr(value: &str) -> (*const c_char, usize) {
    (value.as_ptr().cast::<c_char>(), value.len())
}

fn cbytes(value: &[u8]) -> (*const u8, usize) {
    (value.as_ptr(), value.len())
}

fn request_prefix(handle: *mut front::abi::FrontAbi, request_id: u64) -> String {
    let len = unsafe { r9p_front_request_prefix_copy(handle, request_id, std::ptr::null_mut(), 0) };
    assert!(len >= 0);
    let mut buf = vec![0u8; len as usize];
    let copied =
        unsafe { r9p_front_request_prefix_copy(handle, request_id, buf.as_mut_ptr(), buf.len()) };
    assert_eq!(copied, len);
    String::from_utf8(buf).expect("prefix utf8")
}

fn request_context(handle: *mut front::abi::FrontAbi, request_id: u64) -> String {
    let len =
        unsafe { r9p_front_request_context_copy(handle, request_id, std::ptr::null_mut(), 0) };
    assert!(len >= 0);
    let mut buf = vec![0u8; len as usize];
    let copied =
        unsafe { r9p_front_request_context_copy(handle, request_id, buf.as_mut_ptr(), buf.len()) };
    assert_eq!(copied, len);
    String::from_utf8(buf).expect("context utf8")
}

#[test]
fn abi_roundtrip_over_tcp() {
    assert_eq!(r9p_front_abi_version(), 11);
    let handle = r9p_front_new();
    let (path, path_len) = cstr("market/status");
    let (bytes, bytes_len) = cbytes(b"#M(\"state\" 'open)");
    assert_eq!(
        unsafe { r9p_front_set(handle, path, path_len, bytes, bytes_len) },
        0
    );
    let (events_path, events_len) = cstr("market/events");
    let (seed, seed_len) = cbytes(b"seed\n");
    assert_eq!(
        unsafe { r9p_front_append_event(handle, events_path, events_len, seed, seed_len) },
        0
    );
    let (intake, intake_len) = cstr("queries");
    assert_eq!(
        unsafe { r9p_front_register_intake(handle, intake, intake_len) },
        0
    );
    let (rpc, rpc_len) = cstr("rpc");
    assert_eq!(unsafe { r9p_front_register_rpc(handle, rpc, rpc_len) }, 0);
    let (stream, stream_len) = cstr("stream");
    assert_eq!(
        unsafe { r9p_front_register_log(handle, stream, stream_len) },
        0
    );
    let (bind, bind_len) = cstr("127.0.0.1:0");
    let mut port = 0u16;
    assert_eq!(
        unsafe { r9p_front_serve_tcp(handle, bind, bind_len, &mut port) },
        0
    );
    assert_ne!(port, 0);
    let address = format!("127.0.0.1:{port}");

    let mut client = Client::connect_tcp(&address, "claude", "/", 65536).expect("connect front");
    let status_fid = client.walk_path("/market/status").expect("walk status");
    client.open(status_fid, 0).expect("open status");
    let status = client.read(status_fid, 0, 4096).expect("read status");
    assert_eq!(status, b"#M(\"state\" 'open)".to_vec());

    let market_fid = client.walk_path("/market").expect("walk market");
    client.open(market_fid, 0).expect("open market");
    let first_dir_chunk = client.read(market_fid, 0, 96).expect("read market dir");
    assert_eq!(
        decode_dir_entries(&first_dir_chunk)
            .expect("decode first dir chunk")
            .len(),
        1
    );
    let second_dir_chunk = client
        .read(
            market_fid,
            u64::try_from(first_dir_chunk.len()).expect("dir chunk length"),
            4096,
        )
        .expect("read market dir at offset");
    assert_eq!(
        decode_dir_entries(&second_dir_chunk)
            .expect("decode second dir chunk")
            .len(),
        1
    );

    let events_fid = client.walk_path("/market/events").expect("walk events");
    client.open(events_fid, 0).expect("open events");
    let seed_read = client.read(events_fid, 0, 4096).expect("read seed");
    assert_eq!(seed_read, b"seed\n".to_vec());

    let stream_fid = client
        .walk_path("/stream")
        .expect("walk declared-empty log");
    let stream_stat = client.stat(stream_fid).expect("stat declared-empty log");
    assert_eq!(stream_stat.length, 0);
    assert_eq!(stream_stat.mode & DMDIR, 0);

    let waker = unsafe { handle.as_ref() }.map(|_| ());
    assert!(waker.is_some());
    let wake_handle = handle as usize;
    let pusher = thread::spawn(move || {
        thread::sleep(Duration::from_millis(120));
        let revived = wake_handle as *mut front::abi::FrontAbi;
        let (path, path_len) = cstr("market/events");
        let (bytes, bytes_len) = cbytes(b"wake\n");
        unsafe { r9p_front_append_event(revived, path, path_len, bytes, bytes_len) }
    });
    let started = Instant::now();
    let woken = client.read(events_fid, 5, 4096).expect("blocking read");
    assert_eq!(woken, b"wake\n".to_vec());
    assert!(started.elapsed() >= Duration::from_millis(60));
    assert_eq!(pusher.join().expect("pusher join"), 0);

    let new_fid = client.walk_path("/queries/new").expect("walk new");
    client.open(new_fid, 1).expect("open new for write");
    let wrote = client
        .write_once(new_fid, 0, b"#M(\"kind\" \"search\" \"text\" \"Trump\")")
        .expect("write query");
    assert_eq!(
        wrote as usize,
        b"#M(\"kind\" \"search\" \"text\" \"Trump\")".len()
    );
    let second_query = b"#M(\"kind\" \"search\" \"text\" \"Biden\")";
    let wrote = client
        .write_once(new_fid, 0, second_query)
        .expect("write second query");
    assert_eq!(wrote as usize, second_query.len());

    let mut request_id = 0u64;
    let mut request_len = 0usize;
    assert_eq!(
        unsafe { r9p_front_next_request(handle, 1000, &mut request_id, &mut request_len) },
        0
    );
    assert_eq!(request_id, 1);
    let first_request_id = request_id;
    let first_request_len = request_len;
    assert_eq!(
        unsafe { r9p_front_next_request(handle, 1000, &mut request_id, &mut request_len) },
        0
    );
    assert_eq!(request_id, 2);
    assert_eq!(request_prefix(handle, first_request_id), "queries");
    assert_eq!(request_prefix(handle, request_id), "queries");
    let mut second_buf = vec![0u8; request_len];
    let copied = unsafe {
        r9p_front_request_copy(
            handle,
            request_id,
            second_buf.as_mut_ptr(),
            second_buf.len(),
        )
    };
    assert_eq!(copied as usize, request_len);
    assert_eq!(second_buf, second_query.to_vec());
    let mut buf = vec![0u8; first_request_len];
    let copied =
        unsafe { r9p_front_request_copy(handle, first_request_id, buf.as_mut_ptr(), buf.len()) };
    assert_eq!(copied as usize, first_request_len);
    assert_eq!(buf, b"#M(\"kind\" \"search\" \"text\" \"Trump\")".to_vec());

    let (result, result_len) = cbytes(b"#M(\"hits\" (\"will-trump\" ))");
    assert_eq!(
        unsafe {
            r9p_front_complete_request(
                handle,
                intake,
                intake_len,
                first_request_id,
                result,
                result_len,
            )
        },
        0
    );
    let result_fid = client.walk_path("/queries/1/result").expect("walk result");
    client.open(result_fid, 0).expect("open result");
    let result_read = client.read(result_fid, 0, 4096).expect("read result");
    assert_eq!(result_read, b"#M(\"hits\" (\"will-trump\" ))".to_vec());

    let rpc_fid = client.walk_path("/rpc").expect("walk rpc");
    client.open(rpc_fid, 2).expect("open rpc rdwr");
    let rpc_query = b"#M(\"match\" \"World Cup\")";
    let wrote = client
        .write_once(rpc_fid, 0, rpc_query)
        .expect("write rpc request");
    assert_eq!(wrote as usize, rpc_query.len());
    let mut rpc_request_id = 0u64;
    let mut rpc_request_len = 0usize;
    assert_eq!(
        unsafe { r9p_front_next_request(handle, 1000, &mut rpc_request_id, &mut rpc_request_len) },
        0
    );
    assert_eq!(request_prefix(handle, rpc_request_id), "rpc");
    let mut rpc_buf = vec![0u8; rpc_request_len];
    let copied = unsafe {
        r9p_front_request_copy(handle, rpc_request_id, rpc_buf.as_mut_ptr(), rpc_buf.len())
    };
    assert_eq!(copied as usize, rpc_request_len);
    assert_eq!(rpc_buf, rpc_query.to_vec());
    let (rpc_result, rpc_result_len) = cbytes(b"#M(\"count\" 37)");
    assert_eq!(
        unsafe {
            r9p_front_complete_request(
                handle,
                rpc,
                rpc_len,
                rpc_request_id,
                rpc_result,
                rpc_result_len,
            )
        },
        0
    );
    let rpc_response = client
        .read(rpc_fid, 0, 4096)
        .expect("read rpc response on same fid");
    assert_eq!(rpc_response, b"#M(\"count\" 37)".to_vec());

    assert_eq!(unsafe { r9p_front_stop(handle) }, 0);
    unsafe { r9p_front_free(handle) };
}

#[test]
fn abi_publish_reports_last_error() {
    assert_eq!(r9p_front_abi_version(), 11);
    let handle = r9p_front_new();
    let (vault_bind, vault_bind_len) = cstr("127.0.0.1:1");
    let (vault_uname, vault_uname_len) = cstr("codex");
    let (vault_aname, vault_aname_len) = cstr("/");
    let (service, service_len) = cstr("demo");
    let (export_bind, export_bind_len) = cstr("127.0.0.1:19590");
    let (export_uname, export_uname_len) = cstr("codex");
    let (export_aname, export_aname_len) = cstr("/");
    let (root, root_len) = cstr("/");
    let (transport, transport_len) = cstr("bad-transport");
    let (auth, auth_len) = cstr("none");
    let (protocol, protocol_len) = cstr("9P2000");
    let (label, label_len) = cstr("demo");
    let (empty, empty_len) = cstr("");
    let status = unsafe {
        r9p_front_publish_r9p_export(
            handle,
            vault_bind,
            vault_bind_len,
            vault_uname,
            vault_uname_len,
            vault_aname,
            vault_aname_len,
            service,
            service_len,
            export_bind,
            export_bind_len,
            export_uname,
            export_uname_len,
            export_aname,
            export_aname_len,
            root,
            root_len,
            transport,
            transport_len,
            auth,
            auth_len,
            protocol,
            protocol_len,
            label,
            label_len,
            1234,
            65_536,
            empty,
            empty_len,
            empty,
            empty_len,
        )
    };
    assert_eq!(status, -2);
    let len = unsafe { r9p_front_last_error(handle, std::ptr::null_mut(), 0) };
    assert!(len > 0);
    let mut buf = vec![0u8; usize::try_from(len).expect("last error length")];
    let copied = unsafe { r9p_front_last_error(handle, buf.as_mut_ptr(), buf.len()) };
    assert_eq!(copied, len);
    let message = String::from_utf8(buf).expect("last error should be utf-8");
    assert!(message.contains("unknown transport_class bad-transport"));
    unsafe { r9p_front_free(handle) };
}

#[test]
fn abi_reconcile_without_maintainers_is_ok() {
    assert_eq!(r9p_front_abi_version(), 11);
    let handle = r9p_front_new();
    assert_eq!(unsafe { r9p_front_reconcile_r9p_exports(handle) }, 0);
    unsafe { r9p_front_free(handle) };
}

#[test]
fn abi_door_rehearsal_principal_root_and_write_relay() {
    assert_eq!(r9p_front_abi_version(), 11);
    let handle = r9p_front_new();
    let (status_path, status_path_len) = cstr("views/alice/status");
    let (status_body, status_body_len) = cbytes(b"#M(\"served_state\" \"fresh\")");
    assert_eq!(
        unsafe {
            r9p_front_set(
                handle,
                status_path,
                status_path_len,
                status_body,
                status_body_len,
            )
        },
        0
    );
    let (principal, principal_len) = cstr("alice");
    let (root_path, root_path_len) = cstr("views/alice");
    assert_eq!(
        unsafe {
            r9p_front_set_principal_root(handle, principal, principal_len, root_path, root_path_len)
        },
        0
    );
    let (control, control_len) = cstr("views/alice/control");
    assert_eq!(
        unsafe { r9p_front_register_write_relay(handle, control, control_len) },
        0
    );
    let (bind, bind_len) = cstr("127.0.0.1:0");
    let mut port = 0u16;
    assert_eq!(
        unsafe { r9p_front_serve_tcp(handle, bind, bind_len, &mut port) },
        0
    );
    let address = format!("127.0.0.1:{port}");

    let mut alice = Client::connect_tcp(&address, "alice", "/", 65536).expect("connect alice");
    let status_fid = alice.walk_path("/status").expect("walk status");
    alice.open(status_fid, 0).expect("open status");
    assert_eq!(
        alice.read(status_fid, 0, 4096).expect("read status"),
        b"#M(\"served_state\" \"fresh\")".to_vec()
    );
    assert!(Client::connect_tcp(&address, "bob", "/", 65536).is_err());

    let brain_handle = handle as usize;
    let brain = thread::spawn(move || {
        let handle = brain_handle as *mut front::abi::FrontAbi;
        let mut request_id = 0u64;
        let mut request_len = 0usize;
        assert_eq!(
            unsafe { r9p_front_next_request(handle, 1000, &mut request_id, &mut request_len) },
            0
        );
        let prefix = request_prefix(handle, request_id);
        assert_eq!(prefix, "views/alice/control");
        let mut request = vec![0u8; request_len];
        let copied = unsafe {
            r9p_front_request_copy(handle, request_id, request.as_mut_ptr(), request.len())
        };
        assert_eq!(copied as usize, request_len);
        assert_eq!(request, b"#M(\"command\" \"restart\")".to_vec());
        let (prefix_ptr, prefix_len) = cstr(&prefix);
        assert_eq!(
            unsafe {
                r9p_front_complete_write(
                    handle,
                    prefix_ptr,
                    prefix_len,
                    request_id,
                    u32::try_from(request.len()).expect("request length"),
                )
            },
            0
        );
    });
    let control_fid = alice.walk_path("/control").expect("walk control");
    alice.open(control_fid, OWRITE).expect("open control");
    let wrote = alice
        .write_once(control_fid, 0, b"#M(\"command\" \"restart\")")
        .expect("write control");
    assert_eq!(wrote as usize, b"#M(\"command\" \"restart\")".len());
    brain.join().expect("brain join");

    assert_eq!(unsafe { r9p_front_stop(handle) }, 0);
    unsafe { r9p_front_free(handle) };
}

#[test]
fn abi_v11_pushed_metadata_aname_gate_and_request_context() {
    assert_eq!(r9p_front_abi_version(), 11);
    let handle = r9p_front_new();
    assert_eq!(
        unsafe { r9p_front_set_protocol_limits(handle, 65_536, 4096) },
        0
    );

    let (status_path, status_path_len) = cstr("views/alice/status");
    let (status_body, status_body_len) = cbytes(b"#M(\"served_state\" \"fresh\")");
    let (visibility, visibility_len) = cstr("principal:alice");
    let (freshness, freshness_len) = cstr("freshness:status");
    let (wake, wake_len) = cstr("wake:status");
    let (root_path, root_path_len) = cstr("views/alice");
    assert_eq!(
        unsafe {
            r9p_front_set_pushed_directory(
                handle,
                root_path,
                root_path_len,
                4141,
                76,
                122,
                visibility,
                visibility_len,
                freshness,
                freshness_len,
                wake,
                wake_len,
            )
        },
        0
    );
    assert_eq!(
        unsafe {
            r9p_front_set_pushed_file(
                handle,
                status_path,
                status_path_len,
                status_body,
                status_body_len,
                4242,
                77,
                123,
                visibility,
                visibility_len,
                freshness,
                freshness_len,
                wake,
                wake_len,
            )
        },
        0
    );
    let (principal, principal_len) = cstr("alice");
    let (principal_id, principal_id_len) = cstr("human.alice");
    let (aname, aname_len) = cstr("/");
    let bad_aname = "not-admitted";
    assert_eq!(
        unsafe {
            r9p_front_set_principal_class_aname(
                handle,
                principal,
                principal_len,
                principal_id,
                principal_id_len,
                aname,
                aname_len,
                root_path,
                root_path_len,
            )
        },
        0
    );
    let (control, control_len) = cstr("views/alice/control");
    assert_eq!(
        unsafe { r9p_front_register_write_relay(handle, control, control_len) },
        0
    );
    let (bind, bind_len) = cstr("127.0.0.1:0");
    let mut port = 0u16;
    assert_eq!(
        unsafe { r9p_front_serve_tcp(handle, bind, bind_len, &mut port) },
        0
    );
    let address = format!("127.0.0.1:{port}");

    let mut alice = Client::connect_tcp(&address, "alice", "/", 65_536).expect("connect alice");
    assert_eq!(alice.msize(), 65_536);
    assert_eq!(alice.root_qid().path, 4141);
    assert_eq!(alice.root_qid().version, 76);
    let status_fid = alice.walk_path("/status").expect("walk status");
    let qid = alice.open(status_fid, 0).expect("open status");
    assert_eq!(qid.path, 4242);
    assert_eq!(qid.version, 77);
    assert_eq!(
        alice.read(status_fid, 0, 4096).expect("read status"),
        b"#M(\"served_state\" \"fresh\")".to_vec()
    );
    assert!(Client::connect_tcp(&address, "alice", bad_aname, 65_536).is_err());

    let brain_handle = handle as usize;
    let brain = thread::spawn(move || {
        let handle = brain_handle as *mut front::abi::FrontAbi;
        let mut request_id = 0u64;
        let mut request_len = 0usize;
        assert_eq!(
            unsafe { r9p_front_next_request(handle, 1000, &mut request_id, &mut request_len) },
            0
        );
        assert_eq!(request_prefix(handle, request_id), "views/alice/control");
        let context = request_context(handle, request_id);
        assert!(context.contains("\"version\" \"r9p-front-request-context.v1\""));
        assert!(context.contains("\"principal_id\" \"human.alice\""));
        assert!(context.contains("\"uname\" \"alice\""));
        assert!(context.contains("\"aname\" \"/\""));
        assert!(context.contains("\"target_path\" \"/control\""));
        assert!(context.contains("\"offset\" 9"));
        assert!(context.contains("\"open_mode\" 1"));
        assert!(context.contains("\"pushed_generation\" 0"));
        let mut request = vec![0u8; request_len];
        let copied = unsafe {
            r9p_front_request_copy(handle, request_id, request.as_mut_ptr(), request.len())
        };
        assert_eq!(copied as usize, request_len);
        assert_eq!(request, b"#M(\"command\" \"restart\")".to_vec());
        let (prefix_ptr, prefix_len) = cstr("views/alice/control");
        assert_eq!(
            unsafe {
                r9p_front_complete_write(
                    handle,
                    prefix_ptr,
                    prefix_len,
                    request_id,
                    u32::try_from(request.len()).expect("request length"),
                )
            },
            0
        );
    });
    let control_fid = alice.walk_path("/control").expect("walk control");
    alice.open(control_fid, OWRITE).expect("open control");
    let wrote = alice
        .write_once(control_fid, 9, b"#M(\"command\" \"restart\")")
        .expect("write control");
    assert_eq!(wrote as usize, b"#M(\"command\" \"restart\")".len());
    brain.join().expect("brain join");

    assert_eq!(unsafe { r9p_front_stop(handle) }, 0);
    unsafe { r9p_front_free(handle) };
}

#[test]
fn door_rehearsal_serves_pushed_principal_view_and_fails_unknown_principal() {
    let front = Front::new();
    front
        .set(
            "views/alice/status",
            b"#M(\"contract\" \"runtime-door-freshness.v1\" \"served_state\" \"stale\")",
        )
        .expect("push alice status");
    front
        .set("views/bob/status", b"#M(\"served_state\" \"fresh\")")
        .expect("push bob status");
    front
        .set_principal_root("alice", "views/alice")
        .expect("push alice principal root");
    let serve = front.serve_tcp("127.0.0.1:0").expect("serve front");
    let address = serve.addr().to_string();

    let mut alice = Client::connect_tcp(&address, "alice", "/", 65536).expect("connect alice");
    let status_fid = alice.walk_path("/status").expect("walk alice status");
    alice.open(status_fid, 0).expect("open alice status");
    let read_started = Instant::now();
    let status = alice.read(status_fid, 0, 4096).expect("read alice status");
    let read_elapsed = read_started.elapsed();
    println!(
        "door_rehearsal_pushed_read_latency_us {}",
        read_elapsed.as_micros()
    );
    assert!(
        read_elapsed < Duration::from_millis(50),
        "pushed read should be local latency class, got {read_elapsed:?}"
    );
    assert_eq!(
        status,
        b"#M(\"contract\" \"runtime-door-freshness.v1\" \"served_state\" \"stale\")".to_vec()
    );
    assert!(Client::connect_tcp(&address, "bob", "/", 65536).is_err());

    serve.shutdown();
}

#[test]
fn door_rehearsal_relayed_write_returns_count_after_brain_accepts() {
    let front = Front::new();
    front
        .register_write_relay("control")
        .expect("register control relay");
    front
        .set_wait_timeout(Duration::from_secs(5))
        .expect("set wait timeout");
    let serve = front.serve_tcp("127.0.0.1:0").expect("serve front");
    let address = serve.addr().to_string();
    let brain = front.clone();
    let brain_thread = thread::spawn(move || {
        let request = brain
            .next_request(Duration::from_secs(1))
            .expect("next request")
            .expect("write request");
        assert_eq!(request.prefix, "control");
        assert_eq!(request.bytes, b"#M(\"command\" \"restart\")");
        brain
            .complete_write(
                "control",
                request.request_id,
                u32::try_from(request.bytes.len()).expect("request length"),
            )
            .expect("complete write");
    });

    let mut client = Client::connect_tcp(&address, "alice", "/", 65536).expect("connect front");
    let control_fid = client.walk_path("/control").expect("walk control");
    client.open(control_fid, OWRITE).expect("open control");
    let wrote = client
        .write_once(control_fid, 0, b"#M(\"command\" \"restart\")")
        .expect("write control");
    assert_eq!(wrote as usize, b"#M(\"command\" \"restart\")".len());

    brain_thread.join().expect("brain thread join");
    serve.shutdown();
}

#[test]
fn door_rehearsal_relayed_write_reports_unavailable_when_brain_absent() {
    let front = Front::new();
    front
        .register_write_relay("control")
        .expect("register control relay");
    front
        .set_wait_timeout(Duration::from_millis(20))
        .expect("set wait timeout");
    let serve = front.serve_tcp("127.0.0.1:0").expect("serve front");
    let address = serve.addr().to_string();

    let mut client = Client::connect_tcp(&address, "alice", "/", 65536).expect("connect front");
    let control_fid = client.walk_path("/control").expect("walk control");
    client.open(control_fid, OWRITE).expect("open control");
    let error = client
        .write_once(control_fid, 0, b"#M(\"command\" \"restart\")")
        .expect_err("brain-absent relay must be unavailable");
    assert_eq!(error.message(), b"write relay unavailable");
    assert!(front
        .next_request(Duration::from_millis(0))
        .expect("check pending queue")
        .is_none());

    serve.shutdown();
}

#[test]
fn abi_maintain_reports_initial_publish_error() {
    assert_eq!(r9p_front_abi_version(), 11);
    let handle = r9p_front_new();
    let (vault_bind, vault_bind_len) = cstr("127.0.0.1:1");
    let (vault_uname, vault_uname_len) = cstr("codex");
    let (vault_aname, vault_aname_len) = cstr("/");
    let (service, service_len) = cstr("demo");
    let (export_bind, export_bind_len) = cstr("127.0.0.1:19590");
    let (export_uname, export_uname_len) = cstr("codex");
    let (export_aname, export_aname_len) = cstr("/");
    let (root, root_len) = cstr("/");
    let (transport, transport_len) = cstr("tcp");
    let (auth, auth_len) = cstr("none");
    let (protocol, protocol_len) = cstr("9P2000");
    let (label, label_len) = cstr("demo");
    let (empty, empty_len) = cstr("");
    let status = unsafe {
        r9p_front_maintain_r9p_export(
            handle,
            vault_bind,
            vault_bind_len,
            vault_uname,
            vault_uname_len,
            vault_aname,
            vault_aname_len,
            service,
            service_len,
            export_bind,
            export_bind_len,
            export_uname,
            export_uname_len,
            export_aname,
            export_aname_len,
            root,
            root_len,
            transport,
            transport_len,
            auth,
            auth_len,
            protocol,
            protocol_len,
            label,
            label_len,
            1234,
            65_536,
            0,
            empty,
            empty_len,
            empty,
            empty_len,
        )
    };
    assert_eq!(status, -2);
    let len = unsafe { r9p_front_last_error(handle, std::ptr::null_mut(), 0) };
    assert!(len > 0);
    unsafe { r9p_front_free(handle) };
}

#[test]
fn flush_interrupts_blocked_log_read() {
    let front = Front::new();
    front
        .append_event("market/events", b"seed\n")
        .expect("seed events");
    let serve = front.serve_tcp("127.0.0.1:0").expect("serve front");
    let mut stream = TcpStream::connect(serve.addr()).expect("connect front");
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .expect("set read timeout");

    write_tmessage(
        &mut stream,
        &TMessage::Version {
            tag: NOTAG,
            msize: 8192,
            version: b"9P2000".to_vec(),
        },
    )
    .expect("write version");
    assert!(matches!(
        read_rmessage(&mut stream).expect("read version"),
        RMessage::Version { .. }
    ));
    write_tmessage(
        &mut stream,
        &TMessage::Attach {
            tag: 1,
            fid: 1,
            afid: NOFID,
            uname: b"codex".to_vec(),
            aname: b"/".to_vec(),
        },
    )
    .expect("write attach");
    assert!(matches!(
        read_rmessage(&mut stream).expect("read attach"),
        RMessage::Attach { tag: 1, .. }
    ));
    write_tmessage(
        &mut stream,
        &TMessage::Walk {
            tag: 2,
            fid: 1,
            newfid: 2,
            wnames: vec![b"market".to_vec(), b"events".to_vec()],
        },
    )
    .expect("write walk");
    assert!(matches!(
        read_rmessage(&mut stream).expect("read walk"),
        RMessage::Walk { tag: 2, .. }
    ));
    write_tmessage(
        &mut stream,
        &TMessage::Open {
            tag: 3,
            fid: 2,
            mode: 0,
        },
    )
    .expect("write open");
    assert!(matches!(
        read_rmessage(&mut stream).expect("read open"),
        RMessage::Open { tag: 3, .. }
    ));

    write_tmessage(
        &mut stream,
        &TMessage::Read {
            tag: 4,
            fid: 2,
            offset: 5,
            count: 4096,
        },
    )
    .expect("write blocking read");
    thread::sleep(Duration::from_millis(50));
    write_tmessage(&mut stream, &TMessage::Flush { tag: 5, oldtag: 4 }).expect("write flush");
    assert_eq!(
        read_rmessage(&mut stream).expect("read flush"),
        RMessage::Flush { tag: 5 }
    );
    assert!(read_rmessage(&mut stream).is_err());

    serve.shutdown();
}

#[test]
fn clunk_interrupts_blocked_log_read() {
    let front = Front::new();
    front
        .append_event("market/events", b"seed\n")
        .expect("seed events");
    let serve = front.serve_tcp("127.0.0.1:0").expect("serve front");
    let mut stream = TcpStream::connect(serve.addr()).expect("connect front");
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .expect("set read timeout");

    write_tmessage(
        &mut stream,
        &TMessage::Version {
            tag: NOTAG,
            msize: 8192,
            version: b"9P2000".to_vec(),
        },
    )
    .expect("write version");
    assert!(matches!(
        read_rmessage(&mut stream).expect("read version"),
        RMessage::Version { .. }
    ));
    write_tmessage(
        &mut stream,
        &TMessage::Attach {
            tag: 1,
            fid: 1,
            afid: NOFID,
            uname: b"codex".to_vec(),
            aname: b"/".to_vec(),
        },
    )
    .expect("write attach");
    assert!(matches!(
        read_rmessage(&mut stream).expect("read attach"),
        RMessage::Attach { tag: 1, .. }
    ));
    write_tmessage(
        &mut stream,
        &TMessage::Walk {
            tag: 2,
            fid: 1,
            newfid: 2,
            wnames: vec![b"market".to_vec(), b"events".to_vec()],
        },
    )
    .expect("write walk");
    assert!(matches!(
        read_rmessage(&mut stream).expect("read walk"),
        RMessage::Walk { tag: 2, .. }
    ));
    write_tmessage(
        &mut stream,
        &TMessage::Open {
            tag: 3,
            fid: 2,
            mode: 0,
        },
    )
    .expect("write open");
    assert!(matches!(
        read_rmessage(&mut stream).expect("read open"),
        RMessage::Open { tag: 3, .. }
    ));

    write_tmessage(
        &mut stream,
        &TMessage::Read {
            tag: 4,
            fid: 2,
            offset: 5,
            count: 4096,
        },
    )
    .expect("write blocking read");
    thread::sleep(Duration::from_millis(50));
    write_tmessage(&mut stream, &TMessage::Clunk { tag: 5, fid: 2 }).expect("write clunk");

    let mut saw_read_cancel = false;
    let mut saw_clunk = false;
    for _ in 0..2 {
        match read_rmessage(&mut stream).expect("read clunk/read cancellation") {
            RMessage::Error { tag: 4, ename } => {
                assert_eq!(ename, b"request flushed".to_vec());
                saw_read_cancel = true;
            }
            RMessage::Clunk { tag: 5 } => {
                saw_clunk = true;
            }
            other => panic!("unexpected response after clunking blocked read: {other:?}"),
        }
    }
    assert!(saw_read_cancel);
    assert!(saw_clunk);

    front
        .append_event("market/events", b"after-clunk\n")
        .expect("append after clunk");
    assert!(read_rmessage(&mut stream).is_err());

    serve.shutdown();
}

fn write_tmessage(stream: &mut TcpStream, message: &TMessage) -> Result<(), Error> {
    let frame = codec::encode_tmessage(message)?;
    stream
        .write_all(&frame)
        .map_err(|error| Error::new(format!("write 9P frame: {error}")))?;
    stream
        .flush()
        .map_err(|error| Error::new(format!("flush 9P frame: {error}")))
}

fn read_rmessage(stream: &mut TcpStream) -> Result<RMessage, Error> {
    let mut prefix = [0_u8; 4];
    stream
        .read_exact(&mut prefix)
        .map_err(|error| Error::new(format!("read 9P frame size: {error}")))?;
    let size = u32::from_le_bytes(prefix);
    let rest_len = usize::try_from(size.saturating_sub(4))
        .map_err(|_| Error::from_static("oversized 9P frame"))?;
    let mut frame = Vec::with_capacity(rest_len + 4);
    frame.extend(prefix);
    frame.resize(rest_len + 4, 0);
    stream
        .read_exact(&mut frame[4..])
        .map_err(|error| Error::new(format!("read 9P frame body: {error}")))?;
    codec::decode_rmessage(&frame)
}
