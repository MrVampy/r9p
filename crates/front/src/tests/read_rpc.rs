use super::*;

#[test]
fn prefix_wait_tied_to_rpc_close_does_not_consume_later_requests() -> Result<()> {
    let front = Front::new();
    front.register_rpc("control")?;
    front.register_write_relay("relay")?;
    front.set_wait_timeout(Duration::from_secs(5))?;
    let mut control = front.tree();
    control.attach(1, b"owner", b"/")?;
    let control_qids = walk_to(&mut control, 1, 2, &["control"]);
    control.open(2, control_qids[0], ORDWR)?;
    control.write(2, control_qids[0], 0, b"take relay")?;
    let target = control.read_target(2)?;
    assert!(matches!(target, ReadTarget::Response(_, _, _)));
    let control_request = front
        .next_request_for_prefix("control", Duration::from_millis(200))?
        .expect("control rpc request");
    let waiter_front = front.clone();
    let waiter = thread::spawn(move || {
        waiter_front.next_request_for_prefix_while_rpc_pending("relay", control_request.request_id)
    });

    control.clunk(2, control_qids[0])?;
    let abandoned = waiter.join().expect("waiter join")?;
    assert!(abandoned.is_none());

    let writer_front = front.clone();
    let writer = thread::spawn(move || {
        let mut tree = writer_front.tree();
        tree.attach(1, b"alice", b"/")?;
        let relay_qids = walk_to(&mut tree, 1, 2, &["relay"]);
        tree.open(2, relay_qids[0], OWRITE)?;
        let wrote = tree.write(2, relay_qids[0], 0, b"still owned")?;
        tree.clunk(2, relay_qids[0])?;
        Ok::<u32, Error>(wrote)
    });
    let request = front
        .next_request_for_prefix("relay", Duration::from_millis(200))?
        .expect("relay request must remain available");
    assert_eq!(request.bytes, b"still owned");
    front.complete_write(
        "relay",
        request.request_id,
        u32::try_from(request.bytes.len()).expect("request length"),
    )?;
    let wrote = writer.join().expect("writer join")?;
    assert_eq!(wrote as usize, request.bytes.len());
    Ok(())
}

#[test]
fn register_log_declares_an_empty_walkable_log() -> Result<()> {
    let front = Front::new();
    front.register_log("events")?;
    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["events"]);
    assert_eq!(qids.len(), 1);
    let stat = tree.stat(qids[0])?;
    assert_eq!(stat.length, 0);
    assert_eq!(stat.mode & DMDIR, 0);
    tree.open(2, qids[0], OREAD)?;
    front.append_event("events", b"first\n")?;
    let data = tree.read(2, qids[0], 0, 4096)?;
    assert_eq!(data, ReadData::Bytes(b"first\n".to_vec()));
    Ok(())
}

#[test]
fn read_relay_dispatches_each_range_and_consumes_its_response() -> Result<()> {
    let front = Front::new();
    front.register_read_relay("archive/trade/record")?;
    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["archive", "trade", "record"]);
    let qid = *qids.last().expect("read relay qid");
    let stat = tree.stat(qid)?;
    assert_eq!(stat.length, 0);
    assert!(tree.open(2, qid, OREAD).is_ok());
    assert!(tree.open(2, qid, OWRITE).is_err());

    let target = tree.read_target_at(2, 4, 3)?;
    let ReadTarget::Response(request_id, response_offset, consume) = target else {
        panic!("read relay must park a response");
    };
    assert_eq!(response_offset, 0);
    assert!(consume);
    let request = front
        .next_request(Duration::from_millis(200))?
        .expect("read relay request");
    assert_eq!(request.request_id, request_id);
    assert_eq!(request.prefix, "archive/trade/record");
    assert!(request.bytes.is_empty());
    assert_eq!(request.context.offset, 4);
    assert_eq!(request.context.count, 3);
    assert_eq!(request.context.open_mode, OREAD);

    front.complete_request(&request.prefix, request_id, b"567")?;
    let response = front.response_read(request_id, 0, 3, None, consume)?;
    assert_eq!(response, ReadData::Bytes(b"567".to_vec()));
    assert!(front
        .complete_request(&request.prefix, request_id, b"late")
        .is_err());

    let second = tree.read_target_at(2, 7, 3)?;
    let ReadTarget::Response(second_id, _, _) = second else {
        panic!("second range must be a fresh request");
    };
    assert_ne!(second_id, request_id);
    let second_request = front
        .next_request(Duration::from_millis(200))?
        .expect("second read relay request");
    front.complete_request(&second_request.prefix, second_id, b"")?;
    assert_eq!(
        front.response_read(second_id, 0, 3, None, true)?,
        ReadData::Bytes(Vec::new())
    );
    Ok(())
}

#[test]
fn read_relay_rejection_becomes_the_read_error() -> Result<()> {
    let front = Front::new();
    front.register_read_relay("archive/trade/record")?;
    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["archive", "trade", "record"]);
    let qid = *qids.last().expect("read relay qid");
    tree.open(2, qid, OREAD)?;
    let target = tree.read_target_at(2, 0, 4096)?;
    let ReadTarget::Response(request_id, response_offset, consume) = target else {
        panic!("read relay must park a response");
    };
    let request = front
        .next_request(Duration::from_millis(200))?
        .expect("read relay request");
    front.reject_request(&request.prefix, request_id, "archive record unavailable")?;
    let error = front
        .response_read(request_id, response_offset, 4096, None, consume)
        .expect_err("rejected read relay");
    assert_eq!(error.to_string(), "archive record unavailable");
    Ok(())
}

#[test]
fn snapshot_read_relay_pins_one_response_through_explicit_eof() -> Result<()> {
    let front = Front::new();
    front.register_snapshot_read_relay("status")?;
    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["status"]);
    let qid = *qids.last().expect("snapshot read relay qid");
    tree.open(2, qid, OREAD)?;

    let first = tree.read_target_at(2, 0, 3)?;
    let ReadTarget::Response(request_id, response_offset, consume) = first else {
        panic!("snapshot read relay must park one response");
    };
    assert_eq!(response_offset, 0);
    assert!(!consume);
    let request = front
        .next_request(Duration::from_millis(200))?
        .expect("snapshot read relay request");
    assert_eq!(request.request_id, request_id);
    front.complete_request(&request.prefix, request_id, b"abcdef")?;
    assert!(front
        .complete_request(&request.prefix, request_id, b"replacement")
        .is_err());
    assert_eq!(
        front.response_read(request_id, 0, 3, None, consume)?,
        ReadData::Bytes(b"abc".to_vec())
    );

    let second = tree.read_target_at(2, 3, 3)?;
    let ReadTarget::Response(second_id, second_offset, second_consume) = second else {
        panic!("snapshot continuation must reuse its response");
    };
    assert_eq!(second_id, request_id);
    assert_eq!(second_offset, 3);
    assert!(!second_consume);
    assert_eq!(
        front.response_read(second_id, second_offset, 3, None, second_consume)?,
        ReadData::Bytes(b"def".to_vec())
    );

    let eof = tree.read_target_at(2, 6, 3)?;
    let ReadTarget::Response(eof_id, eof_offset, eof_consume) = eof else {
        panic!("snapshot EOF must reuse its response");
    };
    assert_eq!(eof_id, request_id);
    assert_eq!(
        front.response_read(eof_id, eof_offset, 3, None, eof_consume)?,
        ReadData::Bytes(Vec::new())
    );
    assert!(front.next_request(Duration::from_millis(0))?.is_none());

    tree.clunk(2, qid)?;
    assert!(front
        .complete_request("status", request_id, b"late")
        .is_err());
    Ok(())
}

#[test]
fn rpc_node_only_opens_read_write() -> Result<()> {
    let front = Front::new();
    front.register_rpc("queries")?;
    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["queries"]);
    assert!(tree.open(2, qids[0], ORDWR).is_ok());
    assert!(tree.open(2, qids[0], OREAD).is_err());
    assert!(tree.open(2, qids[0], OWRITE).is_err());
    Ok(())
}

#[test]
fn rpc_single_fid_request_response_roundtrip() -> Result<()> {
    let front = Front::new();
    front.register_rpc("queries")?;
    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["queries"]);
    tree.open(2, qids[0], ORDWR)?;
    let written = tree.write(2, qids[0], 0, b"find markets")?;
    assert_eq!(written as usize, "find markets".len());
    let target = tree.read_target(2)?;
    assert!(matches!(target, ReadTarget::Response(_, _, _)));
    let request = front
        .next_request(Duration::from_millis(200))?
        .expect("a pending rpc request");
    assert_eq!(request.prefix, "queries");
    assert_eq!(request.bytes, b"find markets");
    front.complete_request("queries", request.request_id, b"{\"hits\":2}")?;
    let response = tree.read(2, qids[0], 0, 4096)?;
    assert_eq!(response, ReadData::Bytes(b"{\"hits\":2}".to_vec()));
    let tail = tree.read(2, qids[0], 6, 4096)?;
    assert_eq!(tail, ReadData::Bytes(b"\":2}".to_vec()));
    Ok(())
}

#[test]
fn rpc_request_carries_the_registered_path() -> Result<()> {
    let front = Front::new();
    front.register_rpc("queries")?;
    front.register_rpc("candidates")?;
    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    let query_qids = walk_to(&mut tree, 1, 2, &["queries"]);
    tree.open(2, query_qids[0], ORDWR)?;
    tree.write(2, query_qids[0], 0, b"browse")?;
    let query_target = tree.read_target(2)?;
    assert!(matches!(query_target, ReadTarget::Response(_, _, _)));
    let candidate_qids = walk_to(&mut tree, 1, 3, &["candidates"]);
    tree.open(3, candidate_qids[0], ORDWR)?;
    tree.write(3, candidate_qids[0], 0, b"scan")?;
    let candidate_target = tree.read_target(3)?;
    assert!(matches!(candidate_target, ReadTarget::Response(_, _, _)));
    let first = front
        .next_request(Duration::from_millis(200))?
        .expect("first request");
    let second = front
        .next_request(Duration::from_millis(200))?
        .expect("second request");
    assert_eq!(first.prefix, "queries");
    assert_eq!(first.bytes, b"browse");
    assert_eq!(second.prefix, "candidates");
    assert_eq!(second.bytes, b"scan");
    Ok(())
}

#[test]
fn rpc_read_before_write_is_an_error() -> Result<()> {
    let front = Front::new();
    front.register_rpc("queries")?;
    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["queries"]);
    tree.open(2, qids[0], ORDWR)?;
    assert!(tree.read(2, qids[0], 0, 4096).is_err());
    Ok(())
}

#[test]
fn rpc_second_request_on_same_fid_replaces_the_first() -> Result<()> {
    let front = Front::new();
    front.register_rpc("queries")?;
    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["queries"]);
    tree.open(2, qids[0], ORDWR)?;
    let _ = tree.write(2, qids[0], 0, b"first")?;
    let target = tree.read_target(2)?;
    assert!(matches!(target, ReadTarget::Response(_, _, _)));
    let first = front
        .next_request(Duration::from_millis(200))?
        .expect("first request");
    front.complete_request("queries", first.request_id, b"one")?;
    let _ = tree.write(2, qids[0], 0, b"second")?;
    let target = tree.read_target(2)?;
    assert!(matches!(target, ReadTarget::Response(_, _, _)));
    let second = front
        .next_request(Duration::from_millis(200))?
        .expect("second request");
    assert_eq!(second.prefix, "queries");
    assert_eq!(second.bytes, b"second");
    front.complete_request("queries", second.request_id, b"two")?;
    let response = tree.read(2, qids[0], 0, 4096)?;
    assert_eq!(response, ReadData::Bytes(b"two".to_vec()));
    Ok(())
}

#[test]
fn rpc_buffers_sequential_writes_until_read() -> Result<()> {
    let front = Front::new();
    front.register_rpc("queries")?;
    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["queries"]);
    tree.open(2, qids[0], ORDWR)?;

    let first = vec![b'a'; 4096];
    let second = vec![b'b'; 4096];
    let first_written = tree.write(2, qids[0], 0, &first)?;
    assert_eq!(first_written as usize, first.len());
    assert!(front.next_request(Duration::from_millis(0))?.is_none());
    let second_written = tree.write(2, qids[0], first.len() as u64, &second)?;
    assert_eq!(second_written as usize, second.len());
    assert!(front.next_request(Duration::from_millis(0))?.is_none());

    let target = tree.read_target(2)?;
    assert!(matches!(target, ReadTarget::Response(_, _, _)));
    let request = front
        .next_request(Duration::from_millis(200))?
        .expect("assembled rpc request");
    let mut expected = first;
    expected.extend(second);
    assert_eq!(request.bytes, expected);
    front.complete_request("queries", request.request_id, b"ok")?;
    let response = tree.read(2, qids[0], 0, 4096)?;
    assert_eq!(response, ReadData::Bytes(b"ok".to_vec()));
    Ok(())
}
