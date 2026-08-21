use super::*;
use r9p::error::EPERM;

fn metadata(qid_path: u64) -> PushedDirectoryMetadata {
    PushedDirectoryMetadata {
        qid_path,
        qid_version: 17,
        generation: 23,
        mtime: 1_700_000_123,
        length: 0,
        visibility_class: "catalog-reader".to_string(),
        freshness_ref: "freshness:search".to_string(),
        wake_token: "wake:search".to_string(),
    }
}

fn spawn_walk(front: Front, fid: Fid) -> thread::JoinHandle<Result<(FrontTree, Vec<Qid>)>> {
    thread::spawn(move || {
        let mut tree = front.tree();
        let root = tree.attach(fid, b"reader", b"/")?;
        let qids = tree.walk(
            fid,
            fid + 100,
            root,
            &[b"search".to_vec(), b"rust".to_vec()],
        )?;
        Ok((tree, qids))
    })
}

fn spawn_child_walk(front: Front, fid: Fid) -> thread::JoinHandle<Result<Vec<Qid>>> {
    thread::spawn(move || {
        let mut tree = front.tree();
        let root = tree.attach(fid, b"reader", b"/")?;
        let search_fid = fid + 100;
        let search = tree.walk(fid, search_fid, root, &[b"search".to_vec()])?;
        tree.walk(search_fid, fid + 200, search[0], &[b"rust".to_vec()])
    })
}

#[test]
fn unknown_walk_resolves_directory_before_its_first_read() -> Result<()> {
    let front = Front::new();
    front.set_wait_timeout(Duration::from_secs(2))?;
    front.register_child_directory_resolver(
        "search",
        "search/resolve",
        "search/read",
        ChildDirectoryRemoval::Forbidden,
    )?;

    let walk = spawn_walk(front.clone(), 1);
    let request = front
        .next_request_for_prefix("search/resolve", Duration::from_secs(1))?
        .expect("unknown child resolution request");
    assert_eq!(request.bytes, b"rust");
    assert_eq!(request.context.front_path, "/search/rust");
    assert_eq!(request.context.target_path, "/search/rust");
    assert_eq!(request.context.offset, 0);
    assert_eq!(request.context.count, 0);
    assert!(front
        .next_request_for_prefix("search/read", Duration::from_millis(0))?
        .is_none());

    front.complete_child_directory_resolution(
        "search/resolve",
        request.request_id,
        metadata(9001),
    )?;
    let (mut tree, qids) = walk.join().expect("walk thread")?;
    assert_eq!(qids.len(), 2);
    assert_eq!(qids[1], Qid::new(QTDIR, 17, 9001));
    let stat = tree.stat(qids[1])?;
    assert_eq!(stat.mtime, 1_700_000_123);
    assert_eq!(stat.length, 0);
    assert!(front.next_request(Duration::from_millis(0))?.is_none());

    tree.open(101, qids[1], OREAD)?;
    let target = tree.read_target_at(101, 0, 4096)?;
    assert!(matches!(target, ReadTarget::DirectoryResponse { .. }));
    let read_request = front
        .next_request_for_prefix("search/read", Duration::from_secs(1))?
        .expect("directory read request");
    assert_eq!(read_request.bytes, b"");
    assert_eq!(read_request.context.target_path, "/search/rust");
    Ok(())
}

#[test]
fn concurrent_walks_coalesce_one_child_resolution() -> Result<()> {
    let front = Front::new();
    front.set_wait_timeout(Duration::from_secs(2))?;
    front.register_child_directory_resolver(
        "search",
        "search/resolve",
        "search/read",
        ChildDirectoryRemoval::Forbidden,
    )?;

    let first = spawn_walk(front.clone(), 1);
    let second = spawn_walk(front.clone(), 2);
    let request = front
        .next_request_for_prefix("search/resolve", Duration::from_secs(1))?
        .expect("coalesced resolution request");
    assert!(front
        .next_request_for_prefix("search/resolve", Duration::from_millis(100))?
        .is_none());

    front.complete_child_directory_resolution(
        "search/resolve",
        request.request_id,
        metadata(9002),
    )?;
    let (_, first_qids) = first.join().expect("first walk")?;
    let (_, second_qids) = second.join().expect("second walk")?;
    assert_eq!(first_qids[1], Qid::new(QTDIR, 17, 9002));
    assert_eq!(second_qids[1], Qid::new(QTDIR, 17, 9002));
    Ok(())
}

#[test]
fn rejected_child_resolution_leaves_no_walkable_entry() -> Result<()> {
    let front = Front::new();
    front.set_wait_timeout(Duration::from_secs(2))?;
    front.register_child_directory_resolver(
        "search",
        "search/resolve",
        "search/read",
        ChildDirectoryRemoval::Forbidden,
    )?;
    let walk = spawn_child_walk(front.clone(), 1);
    let request = front
        .next_request_for_prefix("search/resolve", Duration::from_secs(1))?
        .expect("resolution request");
    front.reject_child_directory_resolution(
        "search/resolve",
        request.request_id,
        "query rejected",
    )?;
    let error = match walk.join().expect("walk thread") {
        Ok(_) => panic!("rejected resolution must fail walk"),
        Err(error) => error,
    };
    assert_eq!(error.message(), b"query rejected");

    let retry = spawn_child_walk(front.clone(), 2);
    let retry_request = front
        .next_request_for_prefix("search/resolve", Duration::from_secs(1))?
        .expect("rejected child must not be cached");
    front.reject_child_directory_resolution(
        "search/resolve",
        retry_request.request_id,
        "query still rejected",
    )?;
    assert!(retry.join().expect("retry walk").is_err());
    Ok(())
}

#[test]
fn removed_resolver_parent_releases_the_pending_resolution() -> Result<()> {
    let front = Front::new();
    front.set_wait_timeout(Duration::from_secs(2))?;
    front.register_child_directory_resolver(
        "search",
        "search/resolve",
        "search/read",
        ChildDirectoryRemoval::Forbidden,
    )?;

    let walk = spawn_child_walk(front.clone(), 1);
    let request = front
        .next_request_for_prefix("search/resolve", Duration::from_secs(1))?
        .expect("resolution request");
    front.remove_subtree_if_exists("search")?;

    let error = match walk.join().expect("walk thread") {
        Ok(_) => panic!("removed resolver parent must fail walk"),
        Err(error) => error,
    };
    assert_eq!(error.message(), ENOENT.as_bytes());
    assert!(front
        .complete_child_directory_resolution("search/resolve", request.request_id, metadata(9003),)
        .is_err());

    front.register_child_directory_resolver(
        "search",
        "search/resolve",
        "search/read",
        ChildDirectoryRemoval::Forbidden,
    )?;
    let retry = spawn_child_walk(front.clone(), 2);
    let retry_request = front
        .next_request_for_prefix("search/resolve", Duration::from_secs(1))?
        .expect("removed resolution must not survive parent recreation");
    front.reject_child_directory_resolution(
        "search/resolve",
        retry_request.request_id,
        "retry rejected",
    )?;
    assert!(retry.join().expect("retry walk").is_err());
    Ok(())
}

#[test]
fn child_resolution_and_read_prefixes_must_be_distinct() -> Result<()> {
    let front = Front::new();
    let error = front
        .register_child_directory_resolver(
            "search",
            "search/request",
            "search/request",
            ChildDirectoryRemoval::Forbidden,
        )
        .expect_err("ambiguous request kinds must be rejected");
    assert_eq!(
        error.message(),
        b"child resolution and directory read prefixes must be distinct"
    );
    Ok(())
}

#[test]
fn owner_relayed_child_is_removable_immediately_after_walk() -> Result<()> {
    let front = Front::new();
    front.set_wait_timeout(Duration::from_secs(2))?;
    front.register_child_directory_resolver(
        "search",
        "search/resolve",
        "search/read",
        ChildDirectoryRemoval::RelayToOwner,
    )?;

    let walk = spawn_walk(front.clone(), 1);
    let resolution = front
        .next_request_for_prefix("search/resolve", Duration::from_secs(1))?
        .expect("resolution request");
    front.complete_child_directory_resolution(
        "search/resolve",
        resolution.request_id,
        metadata(9100),
    )?;
    let (mut tree, qids) = walk.join().expect("walk thread")?;
    let removal = thread::spawn(move || tree.remove(101, qids[1]));

    let request = front
        .next_request_for_prefix("search/rust", Duration::from_secs(1))?
        .expect("remove request");
    assert_eq!(request.context.front_path, "/search/rust");
    assert_eq!(request.context.target_path, "/search/rust");
    assert_eq!(request.context.count, 0);
    front.complete_remove("search/rust", request.request_id)?;
    removal.join().expect("remove thread")?;

    let retry = spawn_child_walk(front.clone(), 2);
    let retry_request = front
        .next_request_for_prefix("search/resolve", Duration::from_secs(1))?
        .expect("removed child must resolve again");
    front.reject_child_directory_resolution(
        "search/resolve",
        retry_request.request_id,
        "removed query",
    )?;
    assert!(retry.join().expect("retry walk").is_err());
    Ok(())
}

#[test]
fn ordinary_resolved_child_does_not_gain_remove_authority() -> Result<()> {
    let front = Front::new();
    front.set_wait_timeout(Duration::from_secs(2))?;
    front.register_child_directory_resolver(
        "search",
        "search/resolve",
        "search/read",
        ChildDirectoryRemoval::Forbidden,
    )?;
    let walk = spawn_walk(front.clone(), 1);
    let resolution = front
        .next_request_for_prefix("search/resolve", Duration::from_secs(1))?
        .expect("resolution request");
    front.complete_child_directory_resolution(
        "search/resolve",
        resolution.request_id,
        metadata(9101),
    )?;
    let (mut tree, qids) = walk.join().expect("walk thread")?;
    let error = tree
        .remove(101, qids[1])
        .expect_err("ordinary child must remain inert");
    assert_eq!(error.message(), EPERM.as_bytes());
    assert!(front.next_request(Duration::from_millis(0))?.is_none());
    Ok(())
}
