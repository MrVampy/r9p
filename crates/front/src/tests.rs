use super::*;
use r9p::error::{Error, Result};
use r9p::fid::Fid;
use r9p::qid::{Qid, DMDIR, QTDIR, QTFILE};
use r9p::server::{FileTree, ReadData};
use r9p::stat::Stat;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use std::{sync::mpsc, thread};

fn test_intake_request(request_id: u64, prefix: &str, bytes: &[u8]) -> IntakeRequest {
    IntakeRequest {
        request_id,
        prefix: prefix.to_string(),
        bytes: bytes.to_vec(),
        context: RequestContext {
            principal_id: "test.principal".to_string(),
            uname: "test".to_string(),
            aname: "/".to_string(),
            session_id: 1,
            fid: 1,
            front_path: prefix.to_string(),
            target_path: prefix.to_string(),
            offset: 0,
            count: bytes.len() as u32,
            open_mode: OWRITE,
            pushed_generation: 0,
        },
    }
}

fn walk_to(tree: &mut FrontTree, fid: Fid, newfid: Fid, path: &[&str]) -> Vec<Qid> {
    let names: Vec<Vec<u8>> = path.iter().map(|name| name.as_bytes().to_vec()).collect();
    let start = Qid::new(QTDIR, 0, ROOT_ID);
    tree.walk(fid, newfid, start, &names)
        .expect("walk should succeed")
}

#[test]
fn set_then_walk_and_read_roundtrip() -> Result<()> {
    let front = Front::new();
    front.set("market/status", b"#M(\"state\" 'open)")?;
    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["market", "status"]);
    assert_eq!(qids.len(), 2);
    let open = tree.open(2, qids[1], 0)?;
    let data = tree.read(2, open.qid, 0, 4096)?;
    assert_eq!(data, ReadData::Bytes(b"#M(\"state\" 'open)".to_vec()));
    Ok(())
}

#[test]
fn overwrite_bumps_version_and_serves_new_bytes() -> Result<()> {
    let front = Front::new();
    front.set("market/status", b"first")?;
    front.set("market/status", b"second")?;
    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["market", "status"]);
    assert_eq!(qids[1].version, 1);
    let stat = tree.stat(qids[1])?;
    assert_eq!(stat.length, 6);
    let data = tree.read(2, qids[1], 0, 4096)?;
    assert_eq!(data, ReadData::Bytes(b"second".to_vec()));
    Ok(())
}

#[test]
fn pushed_file_uses_owner_qid_and_version() -> Result<()> {
    let front = Front::new();
    front.set_pushed_file(
        "market/status",
        b"first",
        PushedFileMetadata {
            qid_path: 9001,
            qid_version: 44,
            generation: 100,
            visibility_class: "runtime-reader".to_string(),
            freshness_ref: "freshness:status".to_string(),
            wake_token: "wake:status".to_string(),
        },
    )?;
    front.set_pushed_file(
        "market/status",
        b"second",
        PushedFileMetadata {
            qid_path: 9001,
            qid_version: 45,
            generation: 101,
            visibility_class: "runtime-reader".to_string(),
            freshness_ref: "freshness:status".to_string(),
            wake_token: "wake:status".to_string(),
        },
    )?;
    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["market", "status"]);
    assert_eq!(qids[1].path, 9001);
    assert_eq!(qids[1].version, 45);
    let stat = tree.stat(qids[1])?;
    assert_eq!(stat.qid.path, 9001);
    assert_eq!(stat.qid.version, 45);
    let data = tree.read(2, qids[1], 0, 4096)?;
    assert_eq!(data, ReadData::Bytes(b"second".to_vec()));
    Ok(())
}

#[test]
fn pushed_directory_uses_owner_qid_and_version() -> Result<()> {
    let front = Front::new();
    front.set_pushed_directory(
        "views/core",
        PushedDirectoryMetadata {
            qid_path: 8001,
            qid_version: 7,
            generation: 70,
            visibility_class: "runtime-reader".to_string(),
            freshness_ref: "freshness:core".to_string(),
            wake_token: "wake:core".to_string(),
        },
    )?;
    front.set_pushed_file(
        "views/core/status",
        b"ok",
        PushedFileMetadata {
            qid_path: 9001,
            qid_version: 8,
            generation: 71,
            visibility_class: "runtime-reader".to_string(),
            freshness_ref: "freshness:core/status".to_string(),
            wake_token: "wake:core/status".to_string(),
        },
    )?;
    front.set_principal_class_aname("alice", "principal.alice", "door-token", "views/core")?;

    let mut tree = front.tree();
    let root_qid = tree.attach(1, b"alice", b"door-token")?;
    assert_eq!(root_qid.qtype, QTDIR);
    assert_eq!(root_qid.path, 8001);
    assert_eq!(root_qid.version, 7);
    let root_stat = tree.stat(root_qid)?;
    assert_eq!(root_stat.qid.path, 8001);
    assert_eq!(root_stat.qid.version, 7);

    let qids = walk_to(&mut tree, 1, 2, &["status"]);
    assert_eq!(qids[0].path, 9001);
    assert_eq!(qids[0].version, 8);

    front.set_pushed_directory(
        "views/core",
        PushedDirectoryMetadata {
            qid_path: 8001,
            qid_version: 9,
            generation: 72,
            visibility_class: "runtime-reader".to_string(),
            freshness_ref: "freshness:core".to_string(),
            wake_token: "wake:core".to_string(),
        },
    )?;
    let mut updated = front.tree();
    let updated_root = updated.attach(1, b"alice", b"door-token")?;
    assert_eq!(updated_root.path, 8001);
    assert_eq!(updated_root.version, 9);
    Ok(())
}

#[test]
fn remove_subtree_releases_pushed_qids_and_children() -> Result<()> {
    let front = Front::new();
    front.set_pushed_directory(
        "views/core",
        PushedDirectoryMetadata {
            qid_path: 8001,
            qid_version: 1,
            generation: 1,
            visibility_class: "runtime-reader".to_string(),
            freshness_ref: "freshness:core".to_string(),
            wake_token: "wake:core".to_string(),
        },
    )?;
    front.set_pushed_directory(
        "views/core/services",
        PushedDirectoryMetadata {
            qid_path: 8002,
            qid_version: 1,
            generation: 1,
            visibility_class: "runtime-reader".to_string(),
            freshness_ref: "freshness:core/services".to_string(),
            wake_token: "wake:core/services".to_string(),
        },
    )?;
    front.set_principal_class_aname("alice", "principal.alice", "door-token", "views/core")?;
    front.remove_subtree_if_exists("views/core")?;
    front.set_pushed_directory(
        "views/core",
        PushedDirectoryMetadata {
            qid_path: 8001,
            qid_version: 2,
            generation: 2,
            visibility_class: "runtime-reader".to_string(),
            freshness_ref: "freshness:core".to_string(),
            wake_token: "wake:core".to_string(),
        },
    )?;
    front.set_principal_class_aname("alice", "principal.alice", "door-token", "views/core")?;
    front.set_principal_root("claude", "/")?;

    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["views", "core"]);
    assert_eq!(qids.len(), 2);
    assert_eq!(qids[1].path, 8001);
    assert_eq!(qids[1].version, 2);
    let stale = walk_to(&mut tree, 1, 3, &["views", "core", "services"]);
    assert_eq!(stale.len(), 2);

    let mut scoped = front.tree();
    let root_qid = scoped.attach(1, b"alice", b"door-token")?;
    assert_eq!(root_qid.path, 8001);
    assert_eq!(root_qid.version, 2);
    let stale_from_root = walk_to(&mut scoped, 1, 2, &["services"]);
    assert_eq!(stale_from_root.len(), 0);
    Ok(())
}

#[test]
fn remove_subtree_missing_path_is_noop() -> Result<()> {
    let front = Front::new();
    front.remove_subtree_if_exists("views/core")?;
    front.set("status", b"ok")?;
    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["status"]);
    assert_eq!(qids.len(), 1);
    Ok(())
}

#[test]
fn protocol_limits_control_open_iounit() -> Result<()> {
    let front = Front::new();
    front.set_protocol_limits(65_536, 4096)?;
    front.set("market/status", b"x")?;
    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["market", "status"]);
    let opened = tree.open(2, qids[1], OREAD)?;
    assert_eq!(opened.iounit, 4096);
    Ok(())
}

#[test]
fn missing_path_walks_partially() -> Result<()> {
    let front = Front::new();
    front.set("market/status", b"x")?;
    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["market", "absent"]);
    assert_eq!(qids.len(), 1);
    Ok(())
}

#[test]
fn log_appends_and_reads_in_order() -> Result<()> {
    let front = Front::new();
    front.append_event("market/events", b"one\n")?;
    front.append_event("market/events", b"two\n")?;
    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["market", "events"]);
    let data = tree.read(2, qids[1], 0, 4096)?;
    assert_eq!(data, ReadData::Bytes(b"one\ntwo\n".to_vec()));
    let tail = tree.read(2, qids[1], 4, 4096)?;
    assert_eq!(tail, ReadData::Bytes(b"two\n".to_vec()));
    Ok(())
}

#[test]
fn log_window_drops_whole_entries_and_keeps_absolute_offsets() -> Result<()> {
    let front = Front::new();
    front.set_log_capacity(10)?;
    front.append_event("market/events", b"aaaa\n")?;
    front.append_event("market/events", b"bbbb\n")?;
    front.append_event("market/events", b"cccc\n")?;
    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["market", "events"]);
    let stat = tree.stat(qids[1])?;
    assert_eq!(stat.length, 15);
    let data = tree.read(2, qids[1], 5, 4096)?;
    assert_eq!(data, ReadData::Bytes(b"bbbb\ncccc\n".to_vec()));
    let mid = tree.read(2, qids[1], 12, 4096)?;
    assert_eq!(mid, ReadData::Bytes(b"cc\n".to_vec()));
    Ok(())
}

#[test]
fn log_read_behind_window_fails_typed_with_earliest_offset() -> Result<()> {
    let front = Front::new();
    front.set_log_capacity(10)?;
    front.append_event("market/events", b"aaaa\n")?;
    front.append_event("market/events", b"bbbb\n")?;
    front.append_event("market/events", b"cccc\n")?;
    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["market", "events"]);
    let error = tree
        .read(2, qids[1], 0, 4096)
        .expect_err("behind-window read must fail");
    assert_eq!(
        error.message(),
        b"log window passed: earliest retained offset 5"
    );
    Ok(())
}

#[test]
fn log_keeps_a_single_oversized_entry() -> Result<()> {
    let front = Front::new();
    front.set_log_capacity(4)?;
    front.append_event("market/events", b"0123456789\n")?;
    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["market", "events"]);
    let data = tree.read(2, qids[1], 0, 4096)?;
    assert_eq!(data, ReadData::Bytes(b"0123456789\n".to_vec()));
    Ok(())
}

#[test]
fn log_read_at_tail_blocks_until_push() -> Result<()> {
    let front = Front::new();
    front.append_event("market/events", b"seed\n")?;
    front.set_wait_timeout(Duration::from_secs(5))?;
    let pusher = front.clone();
    let handle = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(80));
        pusher.append_event("market/events", b"wake\n")
    });
    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["market", "events"]);
    let started = Instant::now();
    let data = tree.read(2, qids[1], 5, 4096)?;
    assert_eq!(data, ReadData::Bytes(b"wake\n".to_vec()));
    assert!(started.elapsed() >= Duration::from_millis(50));
    handle.join().expect("push thread").expect("push result");
    Ok(())
}

#[test]
fn log_read_at_tail_times_out_empty() -> Result<()> {
    let front = Front::new();
    front.append_event("market/events", b"seed\n")?;
    front.set_wait_timeout(Duration::from_millis(60))?;
    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["market", "events"]);
    let data = tree.read(2, qids[1], 5, 4096)?;
    assert_eq!(data, ReadData::Bytes(Vec::new()));
    Ok(())
}

#[test]
fn intake_write_lands_request_and_completion() -> Result<()> {
    let front = Front::new();
    front.register_intake("queries")?;
    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["queries", "new"]);
    tree.open(2, qids[1], 1)?;
    let wrote = tree.write(2, qids[1], 0, b"#M(\"kind\" \"search\")")?;
    assert_eq!(wrote as usize, b"#M(\"kind\" \"search\")".len());
    let request = front
        .next_request(Duration::from_millis(200))?
        .expect("pending request");
    assert_eq!(request.request_id, 1);
    assert_eq!(request.prefix, "queries");
    assert_eq!(request.bytes, b"#M(\"kind\" \"search\")".to_vec());
    front.complete_request("queries", request.request_id, b"#M(\"hits\" ())")?;
    let qids = walk_to(&mut tree, 1, 3, &["queries", "1", "result"]);
    let data = tree.read(3, qids[2], 0, 4096)?;
    assert_eq!(data, ReadData::Bytes(b"#M(\"hits\" ())".to_vec()));
    let created = walk_to(&mut tree, 1, 4, &["queries", "created"]);
    let marker = tree.read(4, created[1], 0, 64)?;
    assert_eq!(marker, ReadData::Bytes(b"1".to_vec()));
    Ok(())
}

#[test]
fn intake_blocking_request_wait_wakes_on_write() -> Result<()> {
    let front = Front::new();
    front.register_intake("queries")?;
    let worker_front = front.clone();
    let worker = thread::spawn(move || worker_front.next_request_blocking());
    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["queries", "new"]);
    tree.open(2, qids[1], 1)?;
    tree.write(2, qids[1], 0, b"blocked wait wakes")?;
    let request = worker.join().expect("worker joins")?;
    assert_eq!(request.request_id, 1);
    assert_eq!(request.prefix, "queries");
    assert_eq!(request.bytes, b"blocked wait wakes");
    Ok(())
}

#[test]
fn prefix_wait_leaves_other_pending_requests_in_queue() -> Result<()> {
    let front = Front::new();
    {
        let mut state = front.lock()?;
        state
            .pending
            .push_back(test_intake_request(1, "relay", b"relay"));
        state
            .pending
            .push_back(test_intake_request(2, "control", b"control"));
    }

    let control = front
        .next_request_for_prefix("control", Duration::from_millis(0))?
        .expect("control request should be available");
    assert_eq!(control.request_id, 2);
    assert_eq!(control.prefix, "control");
    assert_eq!(control.bytes, b"control");

    let relay = front
        .next_request(Duration::from_millis(0))?
        .expect("relay request must remain queued");
    assert_eq!(relay.request_id, 1);
    assert_eq!(relay.prefix, "relay");
    assert_eq!(relay.bytes, b"relay");
    Ok(())
}

#[test]
fn intake_new_rejects_reads_and_plain_files_reject_writes() -> Result<()> {
    let front = Front::new();
    front.register_intake("queries")?;
    front.set("market/status", b"x")?;
    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    let new_qids = walk_to(&mut tree, 1, 2, &["queries", "new"]);
    assert!(tree.open(2, new_qids[1], 0).is_err());
    let file_qids = walk_to(&mut tree, 1, 3, &["market", "status"]);
    assert!(tree.open(3, file_qids[1], 1).is_err());
    assert!(tree.write(3, file_qids[1], 0, b"nope").is_err());
    Ok(())
}

#[test]
fn open_modes_are_exact_permissions_not_writeish_bits() -> Result<()> {
    let front = Front::new();
    front.register_intake("queries")?;
    front.set("market/status", b"x")?;
    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    let new_qids = walk_to(&mut tree, 1, 2, &["queries", "new"]);
    assert!(tree.open(2, new_qids[1], OWRITE).is_ok());
    assert!(tree.open(2, new_qids[1], 2).is_err());
    assert!(tree.open(2, new_qids[1], 3).is_err());
    assert!(tree.open(2, new_qids[1], OWRITE | OTRUNC).is_err());
    let file_qids = walk_to(&mut tree, 1, 3, &["market", "status"]);
    assert!(tree.open(3, file_qids[1], OREAD).is_ok());
    assert!(tree.open(3, file_qids[1], 3).is_err());
    assert!(tree.open(3, file_qids[1], OREAD | ORCLOSE).is_err());
    Ok(())
}

#[test]
fn tree_fids_are_per_connection() -> Result<()> {
    let front = Front::new();
    front.set("market/status", b"x")?;
    let mut first = front.tree();
    let mut second = front.tree();
    first.attach(1, b"first", b"/")?;
    second.attach(1, b"second", b"/")?;
    let first_qids = walk_to(&mut first, 1, 2, &["market", "status"]);
    first.clunk(1, Qid::dir(ROOT_ID))?;
    let second_qids = walk_to(&mut second, 1, 2, &["market", "status"]);
    assert_eq!(first_qids.len(), 2);
    assert_eq!(second_qids.len(), 2);
    Ok(())
}

#[test]
fn dotdot_walks_to_parent() -> Result<()> {
    let front = Front::new();
    front.set("market/status", b"x")?;
    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["market", "status"]);
    assert_eq!(qids.len(), 2);
    let names = vec![b"..".to_vec(), b"..".to_vec()];
    let back = tree.walk(2, 3, qids[1], &names).expect("dotdot walk");
    assert_eq!(back.len(), 2);
    assert_eq!(back[1].path, ROOT_ID);
    Ok(())
}

#[test]
fn directory_read_lists_children() -> Result<()> {
    let front = Front::new();
    front.set("market/status", b"x")?;
    front.set("market/events", b"y")?;
    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["market"]);
    let data = tree.read(2, qids[0], 0, 4096)?;
    match data {
        ReadData::Directory(stats) => {
            assert_eq!(stats.len(), 2);
        }
        ReadData::Bytes(_) => panic!("expected directory listing"),
    }
    Ok(())
}

#[test]
fn root_directory_reports_dmdir_and_lists_top_level_children() -> Result<()> {
    let front = Front::new();
    front.set("manifest", b"m")?;
    front.set("state", b"s")?;
    front.register_rpc("queries")?;
    front.append_event("events", b"e\n")?;
    let mut tree = front.tree();
    let root_qid = tree.attach(1, b"claude", b"/")?;
    let stat = tree.stat(root_qid)?;
    assert_ne!(stat.mode & DMDIR, 0);
    assert_eq!(stat.name, b".".to_vec());
    match tree.read(1, root_qid, 0, 4096)? {
        ReadData::Directory(stats) => {
            let mut names: Vec<Vec<u8>> = stats.iter().map(|stat| stat.name.clone()).collect();
            names.sort();
            assert_eq!(
                names,
                vec![
                    b"events".to_vec(),
                    b"manifest".to_vec(),
                    b"queries".to_vec(),
                    b"state".to_vec(),
                ]
            );
        }
        ReadData::Bytes(_) => panic!("expected directory listing for root"),
    }
    Ok(())
}

#[test]
fn pushed_principal_roots_select_views_and_fail_closed() -> Result<()> {
    let front = Front::new();
    front.set("views/alice/status", b"alice-visible")?;
    front.set("views/bob/status", b"bob-visible")?;
    front.set_principal_root_aname("alice", "/", "views/alice")?;

    let mut alice = front.tree();
    alice.attach(1, b"alice", b"/")?;
    let status = walk_to(&mut alice, 1, 2, &["status"]);
    assert_eq!(status.len(), 1);
    let data = alice.read(2, status[0], 0, 4096)?;
    assert_eq!(data, ReadData::Bytes(b"alice-visible".to_vec()));

    let escape = walk_to(&mut alice, 1, 3, &["..", "bob", "status"]);
    assert_eq!(escape.len(), 1);

    let mut bob = front.tree();
    let error = bob
        .attach(1, b"bob", b"/")
        .expect_err("principal without pushed root must fail closed");
    assert_eq!(error.message(), b"principal root unavailable");

    let mut wrong_aname = front.tree();
    let error = wrong_aname
        .attach(1, b"alice", b"not-admitted")
        .expect_err("principal without admitted aname must fail closed");
    assert_eq!(error.message(), b"principal aname unavailable");
    Ok(())
}

#[test]
fn retained_principal_roots_drop_stale_unames() -> Result<()> {
    let front = Front::new();
    front.set("views/control/status", b"control-visible")?;
    front.set("views/service/status", b"service-visible")?;
    front.set_principal_root_aname("vault.runtime", "runtime-door-feed", "views/control")?;
    front.set_principal_root_aname("/srv/old", "/", "views/service")?;

    front.retain_principal_roots(["vault.runtime"])?;

    let mut control = front.tree();
    control.attach(1, b"vault.runtime", b"runtime-door-feed")?;
    let status = walk_to(&mut control, 1, 2, &["status"]);
    assert_eq!(status.len(), 1);
    assert_eq!(
        control.read(2, status[0], 0, 4096)?,
        ReadData::Bytes(b"control-visible".to_vec())
    );

    let mut stale = front.tree();
    let error = stale
        .attach(1, b"/srv/old", b"/")
        .expect_err("retained roots must remove stale service callers");
    assert_eq!(error.message(), b"principal root unavailable");
    Ok(())
}

#[test]
fn write_relay_buffers_chunks_until_clunk() -> Result<()> {
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
        tree.open(2, qids[0], OWRITE)?;
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
