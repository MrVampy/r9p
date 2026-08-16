use super::*;
use r9p::error::EPERM;

fn walk_directory(tree: &mut FrontTree, fid: Fid, newfid: Fid, path: &[&str]) -> Qid {
    let qids = walk_to(tree, fid, newfid, path);
    *qids.last().expect("walked directory qid")
}

#[test]
fn cross_parent_rename_is_one_scoped_owner_request() -> Result<()> {
    let front = Front::new();
    front.set_wait_timeout(Duration::from_secs(2))?;
    front.set("library/incoming/item", b"body")?;
    front.set("library/kept/guide", b"guide")?;
    front.register_rename_relay("library", "library/rename")?;

    let rename_front = front.clone();
    let rename = thread::spawn(move || {
        let mut tree = rename_front.tree();
        let root = tree.attach(1, b"curator", b"/")?;
        let olddir = tree.walk(1, 2, root, &[b"library".to_vec(), b"incoming".to_vec()])?;
        let newdir = tree.walk(1, 3, root, &[b"library".to_vec(), b"kept".to_vec()])?;
        tree.rename_at(
            2,
            *olddir.last().expect("old parent"),
            b"item",
            3,
            *newdir.last().expect("new parent"),
            b"renamed",
        )
    });

    let request = front
        .next_rename_request_for_prefix("library/rename", Duration::from_secs(1))?
        .expect("rename relay request");
    assert_eq!(request.prefix, "library/rename");
    assert_eq!(request.old_name, b"item");
    assert_eq!(request.new_name, b"renamed");
    assert_eq!(request.old_parent.principal_id, "curator");
    assert_eq!(request.old_parent.front_path, "/library/incoming");
    assert_eq!(request.old_parent.target_path, "/library/incoming");
    assert_eq!(request.new_parent.front_path, "/library/kept");
    assert_eq!(request.new_parent.target_path, "/library/kept");
    assert_eq!(request.old_parent.fid, 2);
    assert_eq!(request.new_parent.fid, 3);
    assert!(front
        .complete_rename("another/rename", request.request_id)
        .is_err());
    front.complete_rename("library/rename", request.request_id)?;
    rename.join().expect("rename thread")?;
    Ok(())
}

#[test]
fn rename_cannot_cross_its_registered_owner_root() -> Result<()> {
    let front = Front::new();
    front.set("library/incoming/item", b"body")?;
    front.set("archive/guide", b"guide")?;
    front.register_rename_relay("library", "library/rename")?;

    let mut tree = front.tree();
    let root = tree.attach(1, b"curator", b"/")?;
    let olddir = walk_directory(&mut tree, 1, 2, &["library", "incoming"]);
    let newdir = walk_directory(&mut tree, 1, 3, &["archive"]);
    let error = tree
        .rename_at(2, olddir, b"item", 3, newdir, b"item")
        .expect_err("owner root crossing must fail");
    assert_eq!(error.message(), EPERM.as_bytes());
    assert!(front
        .next_rename_request_for_prefix("library/rename", Duration::from_millis(0))?
        .is_none());
    Ok(())
}

#[test]
fn rejected_rename_preserves_the_owner_error() -> Result<()> {
    let front = Front::new();
    front.set_wait_timeout(Duration::from_secs(2))?;
    front.set("library/incoming/item", b"body")?;
    front.set("library/kept/guide", b"guide")?;
    front.register_rename_relay("library", "library/rename")?;

    let rename_front = front.clone();
    let rename = thread::spawn(move || {
        let mut tree = rename_front.tree();
        let root = tree.attach(1, b"curator", b"/")?;
        let olddir = tree.walk(1, 2, root, &[b"library".to_vec(), b"incoming".to_vec()])?;
        let newdir = tree.walk(1, 3, root, &[b"library".to_vec(), b"kept".to_vec()])?;
        tree.rename_at(
            2,
            *olddir.last().expect("old parent"),
            b"item",
            3,
            *newdir.last().expect("new parent"),
            b"renamed",
        )
    });
    let request = front
        .next_rename_request_for_prefix("library/rename", Duration::from_secs(1))?
        .expect("rename relay request");
    front.reject_rename(
        "library/rename",
        request.request_id,
        "curation policy rejected move",
    )?;
    let error = rename
        .join()
        .expect("rename thread")
        .expect_err("rejected rename must fail");
    assert_eq!(error.message(), b"curation policy rejected move");
    Ok(())
}
