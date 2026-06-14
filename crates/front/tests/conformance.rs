use front::abi::{
    r9p_front_abi_version, r9p_front_append_event, r9p_front_complete_request, r9p_front_free,
    r9p_front_last_error, r9p_front_maintain_r9p_export, r9p_front_new, r9p_front_next_request,
    r9p_front_publish_r9p_export, r9p_front_reconcile_r9p_exports, r9p_front_register_intake,
    r9p_front_register_log, r9p_front_register_rpc, r9p_front_request_copy, r9p_front_serve_tcp,
    r9p_front_set, r9p_front_stop,
};
use front::Front;
use r9p::blocking::Client;
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

#[test]
fn abi_roundtrip_over_tcp() {
    assert_eq!(r9p_front_abi_version(), 7);
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
    assert_eq!(r9p_front_abi_version(), 7);
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
    assert_eq!(r9p_front_abi_version(), 7);
    let handle = r9p_front_new();
    assert_eq!(unsafe { r9p_front_reconcile_r9p_exports(handle) }, 0);
    unsafe { r9p_front_free(handle) };
}

#[test]
fn abi_maintain_reports_initial_publish_error() {
    assert_eq!(r9p_front_abi_version(), 7);
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
