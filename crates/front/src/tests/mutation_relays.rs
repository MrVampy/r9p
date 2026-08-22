use super::*;

#[test]
fn write_relay_accepts_truncating_open_and_buffers_chunks_until_clunk() -> Result<()> {
    let front = Front::new();
    front.register_write_relay("control")?;
    front.set_wait_timeout(Duration::from_secs(5))?;
    let writer_front = front.clone();
    let (write_tx, write_rx) = mpsc::channel();
    let (proceed_tx, proceed_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let writer = thread::spawn(move || {
        let mut tree = writer_front.tree();
        tree.attach(1, b"alice", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["control"]);
        assert!(tree.accepts_open_mode(2, OWRITE | OTRUNC)?);
        tree.open(2, qids[0], OWRITE | OTRUNC)?;
        let first = tree.write(2, qids[0], 0, b"#M(\"command\" ")?;
        let second = tree.write(2, qids[0], u64::from(first), b"\"restart\")")?;
        write_tx.send((first, second)).expect("send write result");
        proceed_rx.recv().expect("wait for clunk signal");
        let result = tree.clunk(2, qids[0]);
        done_tx.send(result).expect("send writer result");
        Ok::<(), Error>(())
    });

    let (first, second) = write_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("writer should accept chunks before clunk");
    assert_eq!(first as usize, b"#M(\"command\" ".len());
    assert_eq!(second as usize, b"\"restart\")".len());
    assert!(front.next_request(Duration::from_millis(0))?.is_none());
    proceed_tx.send(()).expect("signal clunk");

    let request = front
        .next_request(Duration::from_millis(200))?
        .expect("write relay request");
    assert_eq!(request.prefix, "control");
    assert_eq!(request.bytes, b"#M(\"command\" \"restart\")");
    assert!(done_rx.recv_timeout(Duration::from_millis(50)).is_err());

    front.complete_write(
        "control",
        request.request_id,
        u32::try_from(request.bytes.len()).expect("request length"),
    )?;
    done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("writer should finish")?;
    writer.join().expect("writer join")?;
    Ok(())
}

#[test]
fn create_relay_returns_backend_qid_and_rebinds_fid_for_write() -> Result<()> {
    let front = Front::new();
    front.register_create_relay("srv")?;
    front.set_wait_timeout(Duration::from_secs(5))?;
    let writer_front = front.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let writer = thread::spawn(move || {
        let mut tree = writer_front.tree();
        tree.attach(1, b"alice", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["srv"]);
        let opened = tree.create(2, qids[0], b"calendar", 0o666, OWRITE)?;
        let wrote = tree.write(2, opened.qid, 0, b"descriptor")?;
        tree.clunk(2, opened.qid)?;
        done_tx
            .send((opened.qid.path, opened.qid.version, wrote))
            .expect("send writer result");
        Ok::<(), Error>(())
    });

    let create = front.next_create_request_for_prefix_blocking("srv")?;
    assert_eq!(create.prefix, "srv");
    assert_eq!(create.name, "calendar");
    assert_eq!(create.perm, 0o666);
    assert_eq!(create.mode, OWRITE);
    assert_eq!(create.context.front_path, "/srv/calendar");
    assert_eq!(create.context.target_path, "/srv/calendar");
    assert!(done_rx.recv_timeout(Duration::from_millis(50)).is_err());

    front.complete_create("srv", create.request_id, QTFILE, 17, 42_000)?;
    let write = front
        .next_request_for_prefix("srv", Duration::from_secs(1))?
        .expect("dynamic write relay request");
    assert_eq!(write.prefix, "srv");
    assert_eq!(write.context.front_path, "/srv/calendar");
    assert_eq!(write.context.target_path, "/srv/calendar");
    assert_eq!(write.bytes, b"descriptor");
    front.complete_write(
        "srv",
        write.request_id,
        u32::try_from(write.bytes.len()).expect("request length"),
    )?;
    let (qid_path, qid_version, wrote) = done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("writer should finish");
    assert_eq!(qid_path, 42_000);
    assert_eq!(qid_version, 17);
    assert_eq!(wrote as usize, b"descriptor".len());
    writer.join().expect("writer join")?;

    front.set_pushed_file(
        "srv/calendar",
        b"service-channel-report",
        PushedFileMetadata {
            qid_path: 42_000,
            qid_version: 18,
            generation: 120,
            mtime: 1_700_000_300,
            length: 22,
            visibility_class: "runtime-reader".to_string(),
            freshness_ref: "freshness:srv/calendar".to_string(),
            wake_token: "wake:srv/calendar".to_string(),
        },
    )?;
    let mut verifier = front.tree();
    verifier.attach(1, b"alice", b"/")?;
    let qids = walk_to(&mut verifier, 1, 2, &["srv", "calendar"]);
    let stat = verifier.stat(qids[1])?;
    assert_eq!(stat.qid.path, 42_000);
    assert_eq!(stat.qid.version, 18);
    assert_eq!(stat.mode & 0o444, 0o444);
    assert_eq!(stat.mode & 0o222, 0o222);
    let opened_read = verifier.open(2, qids[1], OREAD)?;
    let report = verifier.read(2, opened_read.qid, 0, 4096)?;
    assert_eq!(report, ReadData::Bytes(b"service-channel-report".to_vec()));

    let qids = walk_to(&mut verifier, 1, 3, &["srv", "calendar"]);
    verifier.open(3, qids[1], OWRITE)?;
    let relay_front = front.clone();
    let second_writer = thread::spawn(move || {
        let wrote = verifier.write(3, qids[1], 0, b"updated descriptor")?;
        verifier.clunk(3, qids[1])?;
        Ok::<u32, Error>(wrote)
    });
    let update = relay_front
        .next_request_for_prefix("srv", Duration::from_secs(1))?
        .expect("refreshed service channel must still relay writes");
    assert_eq!(update.context.front_path, "/srv/calendar");
    assert_eq!(update.context.target_path, "/srv/calendar");
    assert_eq!(update.bytes, b"updated descriptor");
    front.complete_write(
        "srv",
        update.request_id,
        u32::try_from(update.bytes.len()).expect("request length"),
    )?;
    let wrote = second_writer.join().expect("second writer join")?;
    assert_eq!(wrote as usize, b"updated descriptor".len());
    Ok(())
}

#[test]
fn accepted_directory_create_publishes_children_before_returning() -> Result<()> {
    let front = Front::new();
    front.register_create_relay("requests")?;
    front.set_wait_timeout(Duration::from_secs(5))?;
    let creator_front = front.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let creator = thread::spawn(move || {
        let mut tree = creator_front.tree();
        tree.attach(1, b"alice", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["requests"]);
        let opened = tree.create(2, qids[0], b"job", DMDIR | 0o755, OREAD)?;
        tree.clunk(2, opened.qid)?;
        let qids = walk_to(&mut tree, 1, 3, &["requests", "job", "status"]);
        let status = tree.read(3, qids[2], 0, 4096)?;
        done_tx.send(status).expect("send status");
        Ok::<(), Error>(())
    });

    let create = front.next_create_request_for_prefix_blocking("requests")?;
    front.complete_create_with("requests", create.request_id, QTDIR, 1, 42_100, |front| {
        front.set("requests/job/status", b"accepted")
    })?;
    assert_eq!(
        done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("created directory status"),
        ReadData::Bytes(b"accepted".to_vec())
    );
    creator.join().expect("creator join")?;
    Ok(())
}

#[test]
fn failed_directory_publication_rejects_create_and_removes_the_subtree() -> Result<()> {
    let front = Front::new();
    front.register_create_relay("requests")?;
    front.set_wait_timeout(Duration::from_secs(5))?;
    let creator_front = front.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let creator = thread::spawn(move || {
        let mut tree = creator_front.tree();
        tree.attach(1, b"alice", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["requests"]);
        done_tx
            .send(tree.create(2, qids[0], b"job", DMDIR | 0o755, OREAD))
            .expect("send create result");
        Ok::<(), Error>(())
    });

    let create = front.next_create_request_for_prefix_blocking("requests")?;
    let error = front
        .complete_create_with("requests", create.request_id, QTDIR, 1, 42_101, |front| {
            front.set("requests/job/status", b"partial")?;
            Err(Error::from_static("publication failed"))
        })
        .expect_err("publication failure");
    assert_eq!(error.to_string(), "publication failed");
    assert_eq!(
        done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("create result")
            .expect_err("rejected create")
            .to_string(),
        "publication failed"
    );
    creator.join().expect("creator join")?;

    let mut verifier = front.tree();
    verifier.attach(1, b"alice", b"/")?;
    assert_eq!(walk_to(&mut verifier, 1, 2, &["requests", "job"]).len(), 1);
    Ok(())
}

#[test]
fn create_relay_projects_slash_names_as_nested_paths() -> Result<()> {
    let front = Front::new();
    front.register_create_relay("srv")?;
    front.set_wait_timeout(Duration::from_secs(5))?;
    let writer_front = front.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let writer = thread::spawn(move || {
        let mut tree = writer_front.tree();
        tree.attach(1, b"alice", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["srv"]);
        let opened = tree.create(2, qids[0], b"infra/credentials", 0o666, OWRITE)?;
        let wrote = tree.write(2, opened.qid, 0, b"descriptor")?;
        tree.clunk(2, opened.qid)?;
        done_tx
            .send((opened.qid.path, opened.qid.version, wrote))
            .expect("send writer result");
        Ok::<(), Error>(())
    });

    let create = front.next_create_request_for_prefix_blocking("srv")?;
    assert_eq!(create.prefix, "srv");
    assert_eq!(create.name, "infra/credentials");
    assert_eq!(create.context.front_path, "/srv/infra/credentials");
    assert_eq!(create.context.target_path, "/srv/infra/credentials");
    assert!(done_rx.recv_timeout(Duration::from_millis(50)).is_err());

    front.complete_create("srv", create.request_id, QTFILE, 17, 42_001)?;
    let write = front
        .next_request_for_prefix("srv", Duration::from_secs(1))?
        .expect("dynamic write relay request");
    assert_eq!(write.prefix, "srv");
    assert_eq!(write.context.front_path, "/srv/infra/credentials");
    assert_eq!(write.context.target_path, "/srv/infra/credentials");
    assert_eq!(write.bytes, b"descriptor");
    front.complete_write(
        "srv",
        write.request_id,
        u32::try_from(write.bytes.len()).expect("request length"),
    )?;
    let (qid_path, qid_version, wrote) = done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("writer should finish");
    assert_eq!(qid_path, 42_001);
    assert_eq!(qid_version, 17);
    assert_eq!(wrote as usize, b"descriptor".len());
    writer.join().expect("writer join")?;

    let mut verifier = front.tree();
    verifier.attach(1, b"alice", b"/")?;
    let qids = walk_to(&mut verifier, 1, 2, &["srv", "infra"]);
    let stat = verifier.stat(qids[1])?;
    assert_eq!(stat.mode & DMDIR, DMDIR);
    let qids = walk_to(&mut verifier, 1, 3, &["srv", "infra", "credentials"]);
    let stat = verifier.stat(qids[2])?;
    assert_eq!(stat.qid.path, 42_001);
    assert_eq!(stat.qid.version, 17);

    front.set_pushed_file(
        "srv/infra/credentials",
        b"service-channel-report",
        PushedFileMetadata {
            qid_path: 42_001,
            qid_version: 18,
            generation: 120,
            mtime: 1_700_000_301,
            length: 22,
            visibility_class: "runtime-reader".to_string(),
            freshness_ref: "freshness:srv/infra/credentials".to_string(),
            wake_token: "wake:srv/infra/credentials".to_string(),
        },
    )?;
    let mut verifier = front.tree();
    verifier.attach(1, b"alice", b"/")?;
    let qids = walk_to(&mut verifier, 1, 2, &["srv", "infra"]);
    let stat = verifier.stat(qids[1])?;
    assert_eq!(stat.mode & DMDIR, DMDIR);
    let qids = walk_to(&mut verifier, 1, 3, &["srv", "infra", "credentials"]);
    let stat = verifier.stat(qids[2])?;
    assert_eq!(stat.qid.path, 42_001);
    assert_eq!(stat.qid.version, 18);
    let opened_read = verifier.open(3, qids[2], OREAD)?;
    let report = verifier.read(3, opened_read.qid, 0, 4096)?;
    assert_eq!(report, ReadData::Bytes(b"service-channel-report".to_vec()));
    Ok(())
}

#[test]
fn create_relay_delegates_existing_child_name_to_backend_policy() -> Result<()> {
    let front = Front::new();
    front.set_pushed_directory(
        "registry/reserved",
        PushedDirectoryMetadata {
            qid_path: 8001,
            qid_version: 7,
            generation: 70,
            mtime: 1_700_000_302,
            length: 0,
            visibility_class: "runtime-reader".to_string(),
            freshness_ref: "freshness:registry/reserved".to_string(),
            wake_token: "wake:registry/reserved".to_string(),
        },
    )?;
    front.register_create_relay("registry")?;
    front.set_wait_timeout(Duration::from_secs(5))?;
    let writer_front = front.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let writer = thread::spawn(move || {
        let mut tree = writer_front.tree();
        tree.attach(1, b"alice", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["registry"]);
        let result = tree.create(2, qids[0], b"reserved", 0o666, OWRITE);
        done_tx.send(result).expect("send create result");
        Ok::<(), Error>(())
    });

    let create = front.next_create_request_for_prefix_blocking("registry")?;
    assert_eq!(create.prefix, "registry");
    assert_eq!(create.name, "reserved");
    assert_eq!(create.context.front_path, "/registry/reserved");
    assert_eq!(create.context.target_path, "/registry/reserved");
    front.reject_create("registry", create.request_id, "reserved_name")?;

    let result = done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("writer should finish");
    assert_eq!(
        result.expect_err("create should be rejected").to_string(),
        "reserved_name"
    );
    writer.join().expect("writer join")?;
    Ok(())
}

#[test]
fn write_relay_reports_unavailable_when_owner_is_absent() -> Result<()> {
    let front = Front::new();
    front.register_write_relay("control")?;
    front.set_wait_timeout(Duration::from_millis(20))?;
    let mut tree = front.tree();
    tree.attach(1, b"alice", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["control"]);
    tree.open(2, qids[0], OWRITE)?;
    let wrote = tree.write(2, qids[0], 0, b"#M(\"command\" \"restart\")")?;
    assert_eq!(wrote as usize, b"#M(\"command\" \"restart\")".len());
    let error = tree
        .clunk(2, qids[0])
        .expect_err("write relay without owner must fail");
    assert_eq!(error.message(), b"write relay unavailable");
    assert!(front.next_request(Duration::from_millis(0))?.is_none());
    Ok(())
}

#[test]
fn write_relay_can_return_owner_denial() -> Result<()> {
    let front = Front::new();
    front.register_write_relay("control")?;
    front.set_wait_timeout(Duration::from_secs(5))?;
    let writer_front = front.clone();
    let (write_tx, write_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let writer = thread::spawn(move || {
        let mut tree = writer_front.tree();
        tree.attach(1, b"alice", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["control"]);
        tree.open(2, qids[0], OWRITE)?;
        let wrote = tree.write(2, qids[0], 0, b"#M(\"command\" \"restart\")")?;
        write_tx.send(wrote).expect("send write result");
        let result = tree.clunk(2, qids[0]);
        done_tx.send(result).expect("send writer result");
        Ok::<(), Error>(())
    });

    let wrote = write_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("writer should accept write before clunk");
    assert_eq!(wrote as usize, b"#M(\"command\" \"restart\")".len());
    let request = front
        .next_request(Duration::from_millis(200))?
        .expect("write relay request");
    front.reject_write("control", request.request_id, "authority denied")?;
    let error = done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("writer should finish")
        .expect_err("owner denial should reach writer");
    assert_eq!(error.message(), b"authority denied");
    writer.join().expect("writer join")?;
    Ok(())
}

#[test]
fn remove_relay_deletes_projection_after_owner_accepts() -> Result<()> {
    let front = Front::new();
    front.set("trades/hyperliquid/demo/stale/state", b"closing")?;
    front.register_remove_relay("trades/hyperliquid/demo/stale")?;
    front.set_wait_timeout(Duration::from_secs(5))?;
    let remover_front = front.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let remover = thread::spawn(move || {
        let mut tree = remover_front.tree();
        tree.attach(1, b"alice", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["trades", "hyperliquid", "demo", "stale"]);
        let result = tree.remove(2, qids[3]);
        done_tx.send(result).expect("send remove result");
        Ok::<(), Error>(())
    });

    let request = front
        .next_request(Duration::from_millis(200))?
        .expect("remove relay request");
    assert_eq!(request.prefix, "trades/hyperliquid/demo/stale");
    assert_eq!(request.bytes, b"");
    assert_eq!(request.context.front_path, "/trades/hyperliquid/demo/stale");
    assert_eq!(
        request.context.target_path,
        "/trades/hyperliquid/demo/stale"
    );
    assert!(done_rx.recv_timeout(Duration::from_millis(50)).is_err());

    front.complete_remove("trades/hyperliquid/demo/stale", request.request_id)?;
    done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("remover should finish")?;
    remover.join().expect("remover join")?;

    let mut verifier = front.tree();
    verifier.attach(1, b"alice", b"/")?;
    let qids = walk_to(
        &mut verifier,
        1,
        2,
        &["trades", "hyperliquid", "demo", "stale"],
    );
    assert_eq!(qids.len(), 3);
    Ok(())
}

#[test]
fn open_fid_survives_removal_until_clunk() -> Result<()> {
    let front = Front::new();
    front.set("requests/proof/wait", b"cancelled")?;
    front.register_remove_relay("requests/proof")?;
    front.set_wait_timeout(Duration::from_secs(5))?;

    let mut reader = front.tree();
    reader.attach(1, b"alice", b"/")?;
    let wait_qids = walk_to(&mut reader, 1, 2, &["requests", "proof", "wait"]);
    let wait_qid = wait_qids[2];
    reader.open(2, wait_qid, OREAD)?;

    let remover_front = front.clone();
    let remover = thread::spawn(move || {
        let mut tree = remover_front.tree();
        tree.attach(1, b"alice", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["requests", "proof"]);
        tree.remove(2, qids[1])
    });
    let request = front
        .next_request(Duration::from_millis(200))?
        .expect("remove relay request");
    front.complete_remove("requests/proof", request.request_id)?;
    remover.join().expect("remover join")?;

    let mut verifier = front.tree();
    verifier.attach(1, b"alice", b"/")?;
    assert_eq!(
        walk_to(&mut verifier, 1, 2, &["requests", "proof"]).len(),
        1
    );
    assert_eq!(
        reader.read(2, wait_qid, 0, 4096)?,
        ReadData::Bytes(b"cancelled".to_vec())
    );
    assert_eq!(reader.stat(wait_qid)?.name, b"wait".to_vec());

    reader.clunk(2, wait_qid)?;
    assert!(reader.stat(wait_qid).is_err());
    Ok(())
}

#[test]
fn remove_relay_can_return_owner_denial() -> Result<()> {
    let front = Front::new();
    front.set("trades/hyperliquid/demo/protected/state", b"open")?;
    front.register_remove_relay("trades/hyperliquid/demo/protected")?;
    front.set_wait_timeout(Duration::from_secs(5))?;
    let remover_front = front.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let remover = thread::spawn(move || {
        let mut tree = remover_front.tree();
        tree.attach(1, b"alice", b"/")?;
        let qids = walk_to(
            &mut tree,
            1,
            2,
            &["trades", "hyperliquid", "demo", "protected"],
        );
        let result = tree.remove(2, qids[3]);
        done_tx.send(result).expect("send remove result");
        Ok::<(), Error>(())
    });

    let request = front
        .next_request(Duration::from_millis(200))?
        .expect("remove relay request");
    front.reject_remove(
        "trades/hyperliquid/demo/protected",
        request.request_id,
        "open_node_requires_explicit_operator_choice",
    )?;
    let error = done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("remover should finish")
        .expect_err("owner denial should reach remover");
    assert_eq!(
        error.message(),
        b"open_node_requires_explicit_operator_choice"
    );
    remover.join().expect("remover join")?;

    let mut verifier = front.tree();
    verifier.attach(1, b"alice", b"/")?;
    let qids = walk_to(
        &mut verifier,
        1,
        2,
        &["trades", "hyperliquid", "demo", "protected", "state"],
    );
    assert_eq!(qids.len(), 5);
    Ok(())
}

#[test]
fn wstat_relay_passes_encoded_stat_to_owner() -> Result<()> {
    let front = Front::new();
    front.set("docs/report", b"body")?;
    front.register_wstat_relay("docs/report")?;
    front.set_wait_timeout(Duration::from_secs(5))?;
    let wstat_front = front.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let writer = thread::spawn(move || {
        let mut tree = wstat_front.tree();
        tree.attach(1, b"alice", b"/")?;
        let qids = walk_to(&mut tree, 1, 2, &["docs", "report"]);
        let mut stat = Stat::null_wstat();
        stat.name = b"renamed".to_vec();
        let result = tree.wstat(2, qids[1], &stat);
        done_tx.send(result).expect("send wstat result");
        Ok::<(), Error>(())
    });

    let request = front
        .next_request(Duration::from_millis(200))?
        .expect("wstat relay request");
    assert_eq!(request.prefix, "docs/report");
    let stat = Stat::decode(&request.bytes)?;
    assert_eq!(stat.name, b"renamed".to_vec());
    assert_eq!(request.context.front_path, "/docs/report");
    assert!(done_rx.recv_timeout(Duration::from_millis(50)).is_err());

    front.complete_wstat("docs/report", request.request_id)?;
    done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("wstat should finish")?;
    writer.join().expect("writer join")?;
    Ok(())
}

#[test]
fn dropping_tree_abandons_pending_rpc_response() -> Result<()> {
    let front = Front::new();
    front.register_rpc("queries")?;
    let mut tree = front.tree();
    tree.attach(1, b"alice", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["queries"]);
    tree.open(2, qids[0], ORDWR)?;
    tree.write(2, qids[0], 0, b"find markets")?;
    let target = tree.read_target(2)?;
    assert!(matches!(target, ReadTarget::Response(_, _, _)));
    let request = front
        .next_request(Duration::from_millis(200))?
        .expect("rpc request");
    drop(tree);

    let error = front
        .complete_request("queries", request.request_id, b"late")
        .expect_err("closed rpc request must not become an intake result");
    assert_eq!(error.message(), ENOENT.as_bytes());

    let mut verifier = front.tree();
    verifier.attach(1, b"alice", b"/")?;
    let qids = walk_to(
        &mut verifier,
        1,
        2,
        &["queries", &request.request_id.to_string(), "result"],
    );
    assert!(qids.len() < 3);
    Ok(())
}

#[test]
fn clunk_removes_unclaimed_pending_rpc_request() -> Result<()> {
    let front = Front::new();
    front.register_rpc("queries")?;
    let mut tree = front.tree();
    tree.attach(1, b"alice", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["queries"]);
    tree.open(2, qids[0], ORDWR)?;
    tree.write(2, qids[0], 0, b"find markets")?;
    let target = tree.read_target(2)?;
    assert!(matches!(target, ReadTarget::Response(_, _, _)));

    tree.clunk(2, qids[0])?;

    assert!(front.next_request(Duration::from_millis(0))?.is_none());
    Ok(())
}

#[test]
fn dropping_tree_removes_unclaimed_pending_rpc_request() -> Result<()> {
    let front = Front::new();
    front.register_rpc("queries")?;
    let mut tree = front.tree();
    tree.attach(1, b"alice", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["queries"]);
    tree.open(2, qids[0], ORDWR)?;
    tree.write(2, qids[0], 0, b"find markets")?;
    let target = tree.read_target(2)?;
    assert!(matches!(target, ReadTarget::Response(_, _, _)));

    drop(tree);

    assert!(front.next_request(Duration::from_millis(0))?.is_none());
    Ok(())
}

#[test]
fn rpc_timeout_removes_unclaimed_pending_request() -> Result<()> {
    let front = Front::new();
    front.register_rpc("queries")?;
    front.set_wait_timeout(Duration::from_millis(1))?;
    let mut tree = front.tree();
    tree.attach(1, b"alice", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["queries"]);
    tree.open(2, qids[0], ORDWR)?;
    tree.write(2, qids[0], 0, b"find markets")?;

    let error = tree
        .read(2, qids[0], 0, 4096)
        .expect_err("unanswered rpc must time out");

    assert_eq!(error.message(), b"rpc request timed out awaiting response");
    assert!(front.next_request(Duration::from_millis(0))?.is_none());
    Ok(())
}

#[test]
fn rpc_flush_removes_unclaimed_pending_request() -> Result<()> {
    let front = Front::new();
    front.register_rpc("queries")?;
    let mut tree = front.tree();
    tree.attach(1, b"alice", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["queries"]);
    tree.open(2, qids[0], ORDWR)?;
    tree.write(2, qids[0], 0, b"find markets")?;
    let cancel = AtomicBool::new(true);

    let error = tree
        .read_with_cancel(2, 0, 4096, Some(&cancel))
        .expect_err("flushed rpc must stop waiting");

    assert!(cancel.load(Ordering::SeqCst));
    assert_eq!(error.message(), b"request flushed");
    assert!(front.next_request(Duration::from_millis(0))?.is_none());
    Ok(())
}

#[test]
fn replacing_rpc_removes_unclaimed_previous_request() -> Result<()> {
    let front = Front::new();
    front.register_rpc("queries")?;
    let mut tree = front.tree();
    tree.attach(1, b"alice", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["queries"]);
    tree.open(2, qids[0], ORDWR)?;
    tree.write(2, qids[0], 0, b"first")?;
    let target = tree.read_target(2)?;
    assert!(matches!(target, ReadTarget::Response(_, _, _)));

    tree.write(2, qids[0], 0, b"second")?;
    let target = tree.read_target(2)?;
    assert!(matches!(target, ReadTarget::Response(_, _, _)));

    let request = front
        .next_request(Duration::from_millis(200))?
        .expect("replacement request");
    assert_eq!(request.bytes, b"second");
    assert!(front.next_request(Duration::from_millis(0))?.is_none());
    Ok(())
}
