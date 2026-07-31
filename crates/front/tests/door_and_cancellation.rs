use super::*;

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
fn door_rehearsal_relayed_write_returns_count_after_owner_accepts() {
    let front = Front::new();
    front
        .register_write_relay("control")
        .expect("register control relay");
    front
        .set_wait_timeout(Duration::from_secs(5))
        .expect("set wait timeout");
    let serve = front.serve_tcp("127.0.0.1:0").expect("serve front");
    let address = serve.addr().to_string();
    let owner = front.clone();
    let owner_thread = thread::spawn(move || {
        let request = owner
            .next_request(Duration::from_secs(1))
            .expect("next request")
            .expect("write request");
        assert_eq!(request.prefix, "control");
        assert_eq!(request.bytes, b"#M(\"command\" \"restart\")");
        owner
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
    client.clunk(control_fid).expect("clunk control");

    owner_thread.join().expect("owner thread join");
    serve.shutdown();
}

#[test]
fn door_rehearsal_relayed_write_reports_unavailable_when_owner_absent() {
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
    let wrote = client
        .write_once(control_fid, 0, b"#M(\"command\" \"restart\")")
        .expect("write control");
    assert_eq!(wrote as usize, b"#M(\"command\" \"restart\")".len());
    let error = client
        .clunk(control_fid)
        .expect_err("owner-absent relay must be unavailable");
    assert_eq!(error.message(), b"write relay unavailable");
    assert!(front
        .next_request(Duration::from_millis(0))
        .expect("check pending queue")
        .is_none());

    serve.shutdown();
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
    codec::write_tmessage_checked(stream, codec::MAX_MSIZE, message)
}

fn read_rmessage(stream: &mut TcpStream) -> Result<RMessage, Error> {
    codec::read_rmessage_checked(stream, codec::MAX_MSIZE)?
        .ok_or_else(|| Error::from_static("9P server closed before response"))
}
