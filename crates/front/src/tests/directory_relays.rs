use super::*;

fn encoded_len(stats: &[Stat]) -> u64 {
    stats
        .iter()
        .map(|stat| stat.encode().expect("encode stat").len() as u64)
        .sum()
}

#[test]
fn directory_relay_pages_are_ordered_and_pinned_per_fid() -> Result<()> {
    let front = Front::new();
    front.register_directory_read_relay("catalog")?;
    let mut tree = front.tree();
    tree.attach(1, b"reader", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["catalog"]);
    tree.open(2, qids[0], OREAD)?;

    let first_target = tree.read_target_at(2, 0, 4096)?;
    let ReadTarget::DirectoryResponse {
        request_id,
        fid,
        node,
    } = first_target
    else {
        panic!("an empty relay must request its first page");
    };
    let request = front
        .next_request(Duration::from_millis(200))?
        .expect("first directory request");
    assert_eq!(request.request_id, request_id);
    assert_eq!(request.context.offset, 0);
    front.set("catalog/second/info.json", b"second")?;
    front.set("catalog/first/info.json", b"first")?;
    front.complete_directory_request(
        "catalog",
        request_id,
        &["second".to_string(), "first".to_string()],
        false,
    )?;
    let response = front.directory_response(request_id, None);
    let ReadData::Directory(first_page) =
        tree.apply_directory_response(fid, node, request_id, response)?
    else {
        panic!("directory response must remain typed as a directory");
    };
    assert_eq!(
        first_page
            .iter()
            .map(|stat| stat.name.as_slice())
            .collect::<Vec<_>>(),
        vec![b"second".as_slice(), b"first".as_slice()]
    );

    let next_offset = encoded_len(&first_page);
    let next_target = tree.read_target_at(2, next_offset, 4096)?;
    let ReadTarget::DirectoryResponse {
        request_id,
        fid,
        node,
    } = next_target
    else {
        panic!("reaching the observed end must request another page");
    };
    let request = front
        .next_request(Duration::from_millis(200))?
        .expect("second directory request");
    front.set("catalog/third/info.json", b"third")?;
    front.complete_directory_request("catalog", request_id, &["third".to_string()], true)?;
    let response = front.directory_response(request_id, None);
    let ReadData::Directory(complete) =
        tree.apply_directory_response(fid, node, request_id, response)?
    else {
        panic!("directory response must remain typed as a directory");
    };
    assert_eq!(
        complete
            .iter()
            .map(|stat| stat.name.as_slice())
            .collect::<Vec<_>>(),
        vec![
            b"second".as_slice(),
            b"first".as_slice(),
            b"third".as_slice()
        ]
    );

    let eof_target = tree.read_target_at(2, encoded_len(&complete), 4096)?;
    assert!(matches!(eof_target, ReadTarget::Directory(_)));
    assert!(front.next_request(Duration::from_millis(0))?.is_none());
    Ok(())
}

#[test]
fn directory_completion_requires_published_direct_children() -> Result<()> {
    let front = Front::new();
    front.register_directory_read_relay("catalog")?;
    let mut tree = front.tree();
    tree.attach(1, b"reader", b"/")?;
    let qids = walk_to(&mut tree, 1, 2, &["catalog"]);
    tree.open(2, qids[0], OREAD)?;
    let target = tree.read_target_at(2, 0, 4096)?;
    let ReadTarget::DirectoryResponse { request_id, .. } = target else {
        panic!("directory request expected");
    };
    let _ = front
        .next_request(Duration::from_millis(200))?
        .expect("directory request");
    assert!(front
        .complete_directory_request("catalog", request_id, &["nested/child".to_string()], true,)
        .is_err());
    Ok(())
}
