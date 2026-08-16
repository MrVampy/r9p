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
            mtime: 1_700_000_000,
            length: 5,
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
            mtime: 1_700_000_100,
            length: 12,
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
    assert_eq!(stat.mtime, 1_700_000_100);
    assert_eq!(stat.length, 12);
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
            mtime: 1_700_000_200,
            length: 0,
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
            mtime: 1_700_000_201,
            length: 2,
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
    assert_eq!(root_stat.mtime, 1_700_000_200);
    assert_eq!(root_stat.length, 0);

    let qids = walk_to(&mut tree, 1, 2, &["status"]);
    assert_eq!(qids[0].path, 9001);
    assert_eq!(qids[0].version, 8);

    front.set_pushed_directory(
        "views/core",
        PushedDirectoryMetadata {
            qid_path: 8001,
            qid_version: 9,
            generation: 72,
            mtime: 1_700_000_202,
            length: 0,
            visibility_class: "runtime-reader".to_string(),
            freshness_ref: "freshness:core".to_string(),
            wake_token: "wake:core".to_string(),
        },
    )?;
    let mut updated = front.tree();
    let updated_root = updated.attach(1, b"alice", b"door-token")?;
    assert_eq!(updated_root.path, 8001);
    assert_eq!(updated_root.version, 9);
    let updated_stat = updated.stat(updated_root)?;
    assert_eq!(updated_stat.mtime, 1_700_000_202);
    Ok(())
}

#[test]
fn pushed_directory_rejects_nonzero_logical_length_without_mutation() -> Result<()> {
    let front = Front::new();
    let error = front
        .set_pushed_directory(
            "views/core",
            PushedDirectoryMetadata {
                qid_path: 8001,
                qid_version: 7,
                generation: 70,
                mtime: 1_700_000_200,
                length: 1,
                visibility_class: "runtime-reader".to_string(),
                freshness_ref: "freshness:core".to_string(),
                wake_token: "wake:core".to_string(),
            },
        )
        .expect_err("directories cannot advertise a byte length");
    assert_eq!(error.to_string(), "pushed directory length must be zero");

    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    assert!(tree
        .walk(1, 2, Qid::new(QTDIR, 0, 0), &[b"views".to_vec()])
        .is_err());
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
            mtime: 1,
            length: 0,
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
            mtime: 1,
            length: 0,
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
            mtime: 2,
            length: 0,
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
    let stale_from_root = scoped
        .walk(1, 2, root_qid, &[b"services".to_vec()])
        .expect_err("a stale first element must be refused, not walked short");
    assert_eq!(stale_from_root.message(), ENOENT.as_bytes());
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
fn retain_subtree_paths_keeps_current_branches_and_removes_stale_siblings() -> Result<()> {
    let front = Front::new();
    front.set("views/runtime/status", b"ready")?;
    front.set("views/runtime/services/current/status", b"current")?;
    front.set("views/runtime/services/stale/status", b"stale")?;
    front.set("views/operator/status", b"outside")?;

    front.retain_subtree_paths(
        "views/runtime",
        [
            "views/runtime/status",
            "views/runtime/services/current/status",
        ],
    )?;

    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    assert_eq!(
        walk_to(&mut tree, 1, 2, &["views", "runtime", "status"]).len(),
        3
    );
    assert_eq!(
        walk_to(
            &mut tree,
            1,
            3,
            &["views", "runtime", "services", "current", "status"]
        )
        .len(),
        5
    );
    assert_eq!(
        walk_to(&mut tree, 1, 4, &["views", "operator", "status"]).len(),
        3
    );
    let services = walk_to(&mut tree, 1, 5, &["views", "runtime", "services"]);
    let stale = tree
        .walk(
            5,
            6,
            *services.last().expect("services qid"),
            &[b"stale".to_vec()],
        )
        .expect_err("stale sibling must be removed");
    assert_eq!(stale.message(), ENOENT.as_bytes());
    Ok(())
}

#[test]
fn retain_subtree_paths_rejects_invalid_set_without_mutation() -> Result<()> {
    let front = Front::new();
    front.set("views/runtime/current", b"current")?;
    front.set("views/runtime/stale", b"stale")?;
    front.set("views/operator/status", b"outside")?;

    front
        .retain_subtree_paths(
            "views/runtime",
            ["views/runtime/current", "views/operator/status"],
        )
        .expect_err("an out-of-subtree retained path must be rejected");

    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    assert_eq!(
        walk_to(&mut tree, 1, 2, &["views", "runtime", "stale"]).len(),
        3
    );
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
fn restored_log_keeps_absolute_offsets_across_reconstruction() -> Result<()> {
    let front = Front::new();
    front.register_log_at("market/events", 41)?;
    front.append_event("market/events", b"one\n")?;
    front.append_event("market/events", b"two\n")?;
    let mut tree = front.tree();
    tree.attach(1, b"claude", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["market", "events"]);
    assert_eq!(tree.stat(qids[1])?.length, 49);
    assert_eq!(
        tree.read(2, qids[1], 41, 4096)?,
        ReadData::Bytes(b"one\ntwo\n".to_vec())
    );
    let error = tree
        .read(2, qids[1], 40, 4096)
        .expect_err("pre-window offset must fail");
    assert_eq!(
        error.message(),
        b"log window passed: earliest retained offset 41"
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

mod child_directory_resolution;
mod directory_relays;
mod mutation_relays;
mod read_rpc;
mod rename_relay;
