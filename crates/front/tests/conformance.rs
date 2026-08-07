use front::abi::{
    r9p_front_abi_version, r9p_front_append_event, r9p_front_capabilities, r9p_front_client_read,
    r9p_front_client_rpc, r9p_front_complete_remove, r9p_front_complete_request,
    r9p_front_complete_write, r9p_front_free, r9p_front_new, r9p_front_next_request,
    r9p_front_register_intake, r9p_front_register_log, r9p_front_register_read_relay,
    r9p_front_register_remove_relay, r9p_front_register_rpc,
    r9p_front_register_snapshot_read_relay, r9p_front_register_write_relay,
    r9p_front_request_context_copy, r9p_front_request_copy, r9p_front_request_prefix_copy,
    r9p_front_serve_tcp, r9p_front_serve_tcp_authenticated, r9p_front_set,
    r9p_front_set_principal_class_aname, r9p_front_set_principal_root,
    r9p_front_set_protocol_limits, r9p_front_set_pushed_directory, r9p_front_set_pushed_file,
    r9p_front_stop, ABI_VERSION, CAPABILITIES,
};
use front::Front;
use r9p::blocking::{Client, OWRITE};
use r9p::fid::NOFID;
use r9p::message::{RMessage, TMessage, NOTAG};
use r9p::qid::DMDIR;
use r9p::stat::decode_dir_entries;
use r9p::{codec, Error};
use r9p_auth::{
    authenticate_client_to, generate_key_pair, generate_root_key_pair, write_key_pair, Certificate,
    CertificateBody, ClientConfig, PublicKey, RootKeyPair,
};
use std::ffi::c_char;
use std::fs;
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

static NEXT_AUTH_TEST: AtomicU64 = AtomicU64::new(1);

fn cstr(value: &str) -> (*const c_char, usize) {
    (value.as_ptr().cast::<c_char>(), value.len())
}

fn cbytes(value: &[u8]) -> (*const u8, usize) {
    (value.as_ptr(), value.len())
}

fn assert_front_contract() {
    assert_eq!(r9p_front_abi_version(), ABI_VERSION);
    assert_eq!(r9p_front_capabilities(), CAPABILITIES);
}

fn front_auth_fixture() -> (PathBuf, PathBuf, ClientConfig) {
    let root = std::env::temp_dir().join(format!(
        "r9p-front-auth-test-{}-{}",
        std::process::id(),
        NEXT_AUTH_TEST.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("auth test directory");
    let server = generate_key_pair().expect("server key pair");
    let client = generate_key_pair().expect("client key pair");
    let server_private = root.join("server.key");
    let server_public = root.join("server.pub");
    write_key_pair(&server_private, &server_public, &server).expect("write server key pair");
    let signing_root = generate_root_key_pair().expect("root key pair");
    let server_cert = root.join("server.crt");
    certify(&signing_root, server.public, "front-test")
        .write(&server_cert)
        .expect("write server certificate");
    let server_config = root.join("server.conf");
    fs::write(
        &server_config,
        format!(
            "format r9p-session-auth.v1\nrole server\ndomain front-test\nprivate-key {}\ncertificate {}\nroot {}\n",
            server_private.display(),
            server_cert.display(),
            signing_root.public
        ),
    )
    .expect("write server config");
    let client_config = ClientConfig::certified(
        client.private,
        certify(&signing_root, client.public, "codex"),
        [signing_root.public],
    )
    .expect("client config");
    (root, server_config, client_config)
}

fn certify(root: &RootKeyPair, key: PublicKey, name: &str) -> Certificate {
    Certificate::sign(
        &root.private,
        CertificateBody::new(
            name,
            key,
            Vec::<String>::new(),
            1,
            4_000_000_000,
            root.public,
        )
        .expect("certificate body"),
    )
    .expect("sign certificate")
}

#[test]
fn abi_authenticated_serve_binds_transport_principal_to_attach_uname() {
    assert_front_contract();
    let (root, server_config, client_config) = front_auth_fixture();
    let handle = r9p_front_new();
    let (path, path_len) = cstr("status");
    let (bytes, bytes_len) = cbytes(b"ready\n");
    assert_eq!(
        unsafe { r9p_front_set(handle, path, path_len, bytes, bytes_len) },
        0
    );
    let (bind, bind_len) = cstr("127.0.0.1:0");
    let server_config = server_config.to_string_lossy();
    let (auth_path, auth_path_len) = cstr(&server_config);
    let mut port = 0_u16;
    assert_eq!(
        unsafe {
            r9p_front_serve_tcp_authenticated(
                handle,
                bind,
                bind_len,
                auth_path,
                auth_path_len,
                &mut port,
            )
        },
        0
    );
    assert_ne!(port, 0);

    let address = format!("127.0.0.1:{port}");
    let stream = TcpStream::connect(address).expect("connect authenticated front");
    let stream = authenticate_client_to(
        stream,
        &client_config,
        "front-test",
        "codex",
        Duration::from_secs(2),
    )
    .expect("authenticate front client");
    let mut client =
        Client::connect(stream, "codex", "/", 65_536).expect("attach authenticated front");
    let fid = client.walk_path("/status").expect("walk status");
    client.open(fid, 0).expect("open status");
    assert_eq!(
        client.read(fid, 0, 4096).expect("read status"),
        b"ready\n".to_vec()
    );

    unsafe { r9p_front_free(handle) };
    fs::remove_dir_all(root).expect("remove auth test directory");
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
    assert_front_contract();
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
    let rpc_reader = thread::spawn(move || {
        client
            .read(rpc_fid, 0, 4096)
            .expect("read rpc response on same fid")
    });
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
    let rpc_response = rpc_reader.join().expect("rpc reader join");
    assert_eq!(rpc_response, b"#M(\"count\" 37)".to_vec());

    assert_eq!(unsafe { r9p_front_stop(handle) }, 0);
    unsafe { r9p_front_free(handle) };
}

#[test]
fn abi_client_rpc_writes_and_reads_same_fid() {
    assert_front_contract();
    let handle = r9p_front_new();
    let (rpc, rpc_len) = cstr("rpc");
    assert_eq!(unsafe { r9p_front_register_rpc(handle, rpc, rpc_len) }, 0);
    let (bind, bind_len) = cstr("127.0.0.1:0");
    let mut port = 0u16;
    assert_eq!(
        unsafe { r9p_front_serve_tcp(handle, bind, bind_len, &mut port) },
        0
    );
    let address = format!("127.0.0.1:{port}");
    let handle_for_client = handle as usize;
    let client = thread::spawn(move || {
        let handle = handle_for_client as *mut front::abi::FrontAbi;
        let (endpoint, endpoint_len) = cstr(&address);
        let (uname, uname_len) = cstr("codex");
        let (aname, aname_len) = cstr("/");
        let (path, path_len) = cstr("/rpc");
        let (request, request_len) = cbytes(b"ping");
        let mut response = vec![0_u8; 128];
        let mut response_len = 0_usize;
        let status = unsafe {
            r9p_front_client_rpc(
                handle,
                endpoint,
                endpoint_len,
                uname,
                uname_len,
                aname,
                aname_len,
                path,
                path_len,
                request,
                request_len,
                65_536,
                response.as_mut_ptr(),
                response.len(),
                &mut response_len,
            )
        };
        assert_eq!(status, 0);
        response.truncate(response_len);
        response
    });

    let mut request_id = 0u64;
    let mut request_len = 0usize;
    assert_eq!(
        unsafe { r9p_front_next_request(handle, 1000, &mut request_id, &mut request_len) },
        0
    );
    assert_eq!(request_prefix(handle, request_id), "rpc");
    let mut request = vec![0u8; request_len];
    let copied =
        unsafe { r9p_front_request_copy(handle, request_id, request.as_mut_ptr(), request.len()) };
    assert_eq!(copied as usize, request_len);
    assert_eq!(request, b"ping");
    let (rpc, rpc_len) = cstr("rpc");
    let (reply, reply_len) = cbytes(b"pong");
    assert_eq!(
        unsafe { r9p_front_complete_request(handle, rpc, rpc_len, request_id, reply, reply_len) },
        0
    );

    assert_eq!(client.join().expect("client join"), b"pong".to_vec());
    assert_eq!(unsafe { r9p_front_stop(handle) }, 0);
    unsafe { r9p_front_free(handle) };
}

#[test]
fn rpc_path_buffers_request_larger_than_negotiated_write_payload() {
    assert_front_contract();
    let handle = r9p_front_new();
    let (rpc, rpc_len) = cstr("rpc");
    assert_eq!(unsafe { r9p_front_register_rpc(handle, rpc, rpc_len) }, 0);
    let (bind, bind_len) = cstr("127.0.0.1:0");
    let mut port = 0u16;
    assert_eq!(
        unsafe { r9p_front_serve_tcp(handle, bind, bind_len, &mut port) },
        0
    );
    let address = format!("127.0.0.1:{port}");
    let request = vec![b'x'; 9500];
    let expected = request.clone();
    let client = thread::spawn(move || {
        let mut client = Client::connect_tcp(&address, "codex", "/", 8192).expect("connect front");
        assert_eq!(client.msize(), 8192);
        client.rpc_path("/rpc", &request).expect("rpc path")
    });

    let mut request_id = 0u64;
    let mut request_len = 0usize;
    assert_eq!(
        unsafe { r9p_front_next_request(handle, 1000, &mut request_id, &mut request_len) },
        0
    );
    assert_eq!(request_prefix(handle, request_id), "rpc");
    assert_eq!(request_len, expected.len());
    let mut request = vec![0u8; request_len];
    let copied =
        unsafe { r9p_front_request_copy(handle, request_id, request.as_mut_ptr(), request.len()) };
    assert_eq!(copied as usize, request_len);
    assert_eq!(request, expected);
    let (reply, reply_len) = cbytes(b"accepted");
    assert_eq!(
        unsafe { r9p_front_complete_request(handle, rpc, rpc_len, request_id, reply, reply_len) },
        0
    );

    assert_eq!(client.join().expect("client join"), b"accepted".to_vec());
    assert_eq!(unsafe { r9p_front_stop(handle) }, 0);
    unsafe { r9p_front_free(handle) };
}

#[test]
fn abi_client_read_reads_namespace_file() {
    assert_front_contract();
    let handle = r9p_front_new();
    let (path, path_len) = cstr("gateways/ibkr/api");
    let (body, body_len) =
        cbytes(br#"{"endpoint":{"host":"192.168.0.21","port":4002},"mode":"paper"}"#);
    assert_eq!(
        unsafe { r9p_front_set(handle, path, path_len, body, body_len) },
        0
    );
    let (bind, bind_len) = cstr("127.0.0.1:0");
    let mut port = 0u16;
    assert_eq!(
        unsafe { r9p_front_serve_tcp(handle, bind, bind_len, &mut port) },
        0
    );
    let address = format!("127.0.0.1:{port}");
    let (endpoint, endpoint_len) = cstr(&address);
    let (uname, uname_len) = cstr("codex");
    let (aname, aname_len) = cstr("/");
    let (read_path, read_path_len) = cstr("/gateways/ibkr/api");
    let mut response = vec![0_u8; 128];
    let mut response_len = 0_usize;
    let status = unsafe {
        r9p_front_client_read(
            handle,
            endpoint,
            endpoint_len,
            uname,
            uname_len,
            aname,
            aname_len,
            read_path,
            read_path_len,
            65_536,
            response.as_mut_ptr(),
            response.len(),
            &mut response_len,
        )
    };
    assert_eq!(status, 0);
    response.truncate(response_len);
    assert_eq!(
        response,
        br#"{"endpoint":{"host":"192.168.0.21","port":4002},"mode":"paper"}"#.to_vec()
    );
    assert_eq!(unsafe { r9p_front_stop(handle) }, 0);
    unsafe { r9p_front_free(handle) };
}

#[test]
fn abi_read_relay_forwards_ranges_until_eof() {
    assert_front_contract();
    let handle = r9p_front_new();
    let (path, path_len) = cstr("archive/trade/record");
    assert_eq!(
        unsafe { r9p_front_register_read_relay(handle, path, path_len) },
        0
    );
    let (bind, bind_len) = cstr("127.0.0.1:0");
    let mut port = 0u16;
    assert_eq!(
        unsafe { r9p_front_serve_tcp(handle, bind, bind_len, &mut port) },
        0
    );
    let address = format!("127.0.0.1:{port}");
    let handle_for_client = handle as usize;
    let reader = thread::spawn(move || {
        let handle = handle_for_client as *mut front::abi::FrontAbi;
        let (endpoint, endpoint_len) = cstr(&address);
        let (uname, uname_len) = cstr("codex");
        let (aname, aname_len) = cstr("/");
        let (path, path_len) = cstr("/archive/trade/record");
        let mut response = vec![0_u8; 128];
        let mut response_len = 0_usize;
        let status = unsafe {
            r9p_front_client_read(
                handle,
                endpoint,
                endpoint_len,
                uname,
                uname_len,
                aname,
                aname_len,
                path,
                path_len,
                65_536,
                response.as_mut_ptr(),
                response.len(),
                &mut response_len,
            )
        };
        assert_eq!(status, 0);
        response.truncate(response_len);
        response
    });

    let mut request_id = 0_u64;
    let mut request_len = 0_usize;
    assert_eq!(
        unsafe { r9p_front_next_request(handle, 1000, &mut request_id, &mut request_len) },
        0
    );
    assert_eq!(request_len, 0);
    assert_eq!(request_prefix(handle, request_id), "archive/trade/record");
    let context = request_context(handle, request_id);
    assert!(context.contains("\"version\" \"r9p-front-request-context.v2\""));
    assert!(context.contains("\"offset\" 0"));
    assert!(context.contains("\"count\" "));
    assert_eq!(
        unsafe { r9p_front_request_copy(handle, request_id, std::ptr::null_mut(), 0) },
        0
    );
    let (body, body_len) = cbytes(b"cold");
    assert_eq!(
        unsafe { r9p_front_complete_request(handle, path, path_len, request_id, body, body_len) },
        0
    );

    let mut eof_id = 0_u64;
    let mut eof_len = 0_usize;
    assert_eq!(
        unsafe { r9p_front_next_request(handle, 1000, &mut eof_id, &mut eof_len) },
        0
    );
    assert_eq!(eof_len, 0);
    assert!(request_context(handle, eof_id).contains("\"offset\" 4"));
    assert_eq!(
        unsafe { r9p_front_request_copy(handle, eof_id, std::ptr::null_mut(), 0) },
        0
    );
    assert_eq!(
        unsafe { r9p_front_complete_request(handle, path, path_len, eof_id, std::ptr::null(), 0) },
        0
    );

    assert_eq!(
        reader.join().expect("read relay client join"),
        b"cold".to_vec()
    );
    assert_eq!(unsafe { r9p_front_stop(handle) }, 0);
    unsafe { r9p_front_free(handle) };
}

#[test]
fn abi_snapshot_read_relay_pins_one_full_record_until_eof() {
    assert_front_contract();
    let handle = r9p_front_new();
    let (path, path_len) = cstr("status");
    assert_eq!(
        unsafe { r9p_front_register_snapshot_read_relay(handle, path, path_len) },
        0
    );
    let (bind, bind_len) = cstr("127.0.0.1:0");
    let mut port = 0u16;
    assert_eq!(
        unsafe { r9p_front_serve_tcp(handle, bind, bind_len, &mut port) },
        0
    );
    let address = format!("127.0.0.1:{port}");
    let handle_for_client = handle as usize;
    let reader = thread::spawn(move || {
        let handle = handle_for_client as *mut front::abi::FrontAbi;
        let (endpoint, endpoint_len) = cstr(&address);
        let (uname, uname_len) = cstr("codex");
        let (aname, aname_len) = cstr("/");
        let (path, path_len) = cstr("/status");
        let mut response = vec![0_u8; 128];
        let mut response_len = 0_usize;
        let status = unsafe {
            r9p_front_client_read(
                handle,
                endpoint,
                endpoint_len,
                uname,
                uname_len,
                aname,
                aname_len,
                path,
                path_len,
                65_536,
                response.as_mut_ptr(),
                response.len(),
                &mut response_len,
            )
        };
        assert_eq!(status, 0);
        response.truncate(response_len);
        response
    });

    let mut request_id = 0_u64;
    let mut request_len = 0_usize;
    assert_eq!(
        unsafe { r9p_front_next_request(handle, 1000, &mut request_id, &mut request_len) },
        0
    );
    assert_eq!(request_len, 0);
    assert_eq!(request_prefix(handle, request_id), "status");
    assert_eq!(
        unsafe { r9p_front_request_copy(handle, request_id, std::ptr::null_mut(), 0) },
        0
    );
    let (body, body_len) = cbytes(b"{\"state\":\"ready\"}\n");
    assert_eq!(
        unsafe { r9p_front_complete_request(handle, path, path_len, request_id, body, body_len) },
        0
    );

    assert_eq!(
        reader.join().expect("snapshot read relay client join"),
        b"{\"state\":\"ready\"}\n".to_vec()
    );
    let mut extra_id = 0_u64;
    let mut extra_len = 0_usize;
    assert_eq!(
        unsafe { r9p_front_next_request(handle, 0, &mut extra_id, &mut extra_len) },
        1
    );
    assert_eq!(unsafe { r9p_front_stop(handle) }, 0);
    unsafe { r9p_front_free(handle) };
}

#[test]
fn abi_door_rehearsal_principal_root_and_write_relay() {
    assert_front_contract();
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

    let owner_handle = handle as usize;
    let owner = thread::spawn(move || {
        let handle = owner_handle as *mut front::abi::FrontAbi;
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
    alice.clunk(control_fid).expect("clunk control");
    owner.join().expect("owner join");

    assert_eq!(unsafe { r9p_front_stop(handle) }, 0);
    unsafe { r9p_front_free(handle) };
}

#[test]
fn abi_v11_pushed_metadata_aname_gate_and_request_context() {
    assert_front_contract();
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

    let owner_handle = handle as usize;
    let owner = thread::spawn(move || {
        let handle = owner_handle as *mut front::abi::FrontAbi;
        let mut request_id = 0u64;
        let mut request_len = 0usize;
        assert_eq!(
            unsafe { r9p_front_next_request(handle, 1000, &mut request_id, &mut request_len) },
            0
        );
        assert_eq!(request_prefix(handle, request_id), "views/alice/control");
        let context = request_context(handle, request_id);
        assert!(context.contains("\"version\" \"r9p-front-request-context.v2\""));
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
    alice.clunk(control_fid).expect("clunk control");
    owner.join().expect("owner join");

    assert_eq!(unsafe { r9p_front_stop(handle) }, 0);
    unsafe { r9p_front_free(handle) };
}

#[test]
fn abi_remove_relay_uses_tremove_and_drops_projection() {
    assert_front_contract();
    let handle = r9p_front_new();
    let (state_path, state_path_len) = cstr("trades/demo/trade-1/state");
    let (state_body, state_body_len) = cbytes(br#"{"lifecycle":"archived"}"#);
    assert_eq!(
        unsafe {
            r9p_front_set(
                handle,
                state_path,
                state_path_len,
                state_body,
                state_body_len,
            )
        },
        0
    );
    let (trade_path, trade_path_len) = cstr("trades/demo/trade-1");
    assert_eq!(
        unsafe { r9p_front_register_remove_relay(handle, trade_path, trade_path_len) },
        0
    );
    let (bind, bind_len) = cstr("127.0.0.1:0");
    let mut port = 0u16;
    assert_eq!(
        unsafe { r9p_front_serve_tcp(handle, bind, bind_len, &mut port) },
        0
    );
    let address = format!("127.0.0.1:{port}");

    let client_thread = thread::spawn(move || {
        let mut client =
            Client::connect_tcp(&address, "codex", "/", 65_536).expect("connect front");
        let trade_fid = client
            .walk_path("/trades/demo/trade-1")
            .expect("walk trade");
        client.remove(trade_fid).expect("remove trade");
        assert!(client.walk_path("/trades/demo/trade-1").is_err());
    });

    let mut request_id = 0u64;
    let mut request_len = 0usize;
    assert_eq!(
        unsafe { r9p_front_next_request(handle, 1000, &mut request_id, &mut request_len) },
        0
    );
    assert_eq!(request_prefix(handle, request_id), "trades/demo/trade-1");
    assert_eq!(request_len, 0);
    let context = request_context(handle, request_id);
    assert!(context.contains("\"target_path\" \"/trades/demo/trade-1\""));
    assert_eq!(
        unsafe { r9p_front_request_copy(handle, request_id, std::ptr::null_mut(), 0) },
        0
    );
    let (prefix, prefix_len) = cstr("trades/demo/trade-1");
    assert_eq!(
        unsafe { r9p_front_complete_remove(handle, prefix, prefix_len, request_id) },
        0
    );

    client_thread.join().expect("client join");
    assert_eq!(unsafe { r9p_front_stop(handle) }, 0);
    unsafe { r9p_front_free(handle) };
}

#[path = "conformance/door_and_cancellation.rs"]
mod door_and_cancellation;
