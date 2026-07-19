use super::*;
use r9p::{
    blocking::{OREAD, OTRUNC, OWRITE},
    message::{RMessage, TMessage},
    server::Server,
    stat::decode_dir_entries,
};
use std::{
    env, fs,
    os::unix::{ffi::OsStrExt, fs::symlink},
    process,
    time::{SystemTime, UNIX_EPOCH},
};

#[test]
fn serves_file_reads_by_offset() -> Result<()> {
    let root = fixture_root("read")?;
    fs::write(root.join("body"), b"abcdef").map_err(|error| Error::from(error.to_string()))?;

    let mut server = Server::new(LocalTree::open(&root)?);
    attach(&mut server);
    walk(&mut server, 1, 2, b"body");
    open(&mut server, 2);

    let reply = server.handle(TMessage::Read {
        tag: 4,
        fid: 2,
        offset: 2,
        count: 3,
    });
    assert_eq!(
        reply,
        RMessage::Read {
            tag: 4,
            data: b"cde".to_vec()
        }
    );

    remove_fixture(root);
    Ok(())
}

#[test]
fn lists_regular_directory_entries() -> Result<()> {
    let root = fixture_root("dir")?;
    fs::write(root.join("a"), b"a").map_err(|error| Error::from(error.to_string()))?;
    fs::write(root.join("b"), b"b").map_err(|error| Error::from(error.to_string()))?;

    let mut server = Server::new(LocalTree::open(&root)?);
    attach(&mut server);
    clone_fid(&mut server, 1, 2);
    open(&mut server, 2);
    let reply = server.handle(TMessage::Read {
        tag: 3,
        fid: 2,
        offset: 0,
        count: 8192,
    });
    let data = match reply {
        RMessage::Read { data, .. } => data,
        other => return Err(Error::from(format!("unexpected reply: {other:?}"))),
    };
    let names = decode_dir_entries(&data)?
        .into_iter()
        .map(|stat| stat.name)
        .collect::<Vec<_>>();
    assert_eq!(names, [b"a".to_vec(), b"b".to_vec()]);

    remove_fixture(root);
    Ok(())
}

#[test]
fn rejects_parent_walk_escape() -> Result<()> {
    let root = fixture_root("escape")?;
    let mut server = Server::new(LocalTree::open(&root)?);
    attach(&mut server);
    let reply = server.handle(TMessage::Walk {
        tag: 2,
        fid: 1,
        newfid: 2,
        wnames: vec![b"..".to_vec()],
    });
    assert!(matches!(reply, RMessage::Error { .. }));

    remove_fixture(root);
    Ok(())
}

#[test]
fn serves_symlink_target_without_following_outside_export() -> Result<()> {
    let root = fixture_root("symlink")?;
    let outside = fixture_root("outside")?;
    fs::write(outside.join("secret"), b"secret").map_err(|error| Error::from(error.to_string()))?;
    symlink(outside.join("secret"), root.join("secret-link"))
        .map_err(|error| Error::from(error.to_string()))?;

    let mut server = Server::new(LocalTree::open(&root)?);
    attach(&mut server);
    let reply = server.handle(TMessage::Walk {
        tag: 2,
        fid: 1,
        newfid: 2,
        wnames: vec![b"secret-link".to_vec()],
    });
    assert!(matches!(reply, RMessage::Walk { .. }));
    let reply = server.handle(TMessage::Read {
        tag: 3,
        fid: 2,
        offset: 0,
        count: 8192,
    });
    assert_eq!(
        reply,
        RMessage::Read {
            tag: 3,
            data: outside.join("secret").as_os_str().as_bytes().to_vec()
        }
    );

    remove_fixture(root);
    remove_fixture(outside);
    Ok(())
}

#[test]
fn read_only_export_rejects_writes() -> Result<()> {
    let root = fixture_root("readonly")?;
    fs::write(root.join("body"), b"abcdef").map_err(|error| Error::from(error.to_string()))?;

    let mut server = Server::new(LocalTree::open(&root)?);
    attach(&mut server);
    walk(&mut server, 1, 2, b"body");
    let reply = server.handle(TMessage::Open {
        tag: 3,
        fid: 2,
        mode: OWRITE,
    });
    assert!(matches!(reply, RMessage::Error { .. }));

    remove_fixture(root);
    Ok(())
}

#[test]
fn writable_export_creates_truncates_and_writes() -> Result<()> {
    let root = fixture_root("writable")?;
    fs::write(root.join("body"), b"abcdef").map_err(|error| Error::from(error.to_string()))?;

    let mut server = Server::new(LocalTree::open_with_config(
        &root,
        LocalTreeConfig { writable: true },
    )?);
    attach(&mut server);
    let reply = server.handle(TMessage::Create {
        tag: 2,
        fid: 1,
        name: b"created".to_vec(),
        perm: 0o666,
        mode: OWRITE,
    });
    let created_qid = match reply {
        RMessage::Create { qid, .. } => qid,
        other => return Err(Error::from(format!("unexpected create reply: {other:?}"))),
    };
    let reply = server.handle(TMessage::Write {
        tag: 3,
        fid: 1,
        offset: 0,
        data: b"created\n".to_vec(),
    });
    assert_eq!(reply, RMessage::Write { tag: 3, count: 8 });
    assert_eq!(
        fs::read_to_string(root.join("created")).map_err(|error| Error::from(error.to_string()))?,
        "created\n"
    );
    let reply = server.handle(TMessage::Clunk { tag: 4, fid: 1 });
    assert!(matches!(reply, RMessage::Clunk { .. }));

    attach(&mut server);
    walk(&mut server, 1, 2, b"body");
    let reply = server.handle(TMessage::Open {
        tag: 5,
        fid: 2,
        mode: OWRITE | OTRUNC,
    });
    assert!(matches!(reply, RMessage::Open { .. }));
    let reply = server.handle(TMessage::Write {
        tag: 6,
        fid: 2,
        offset: 0,
        data: b"xy".to_vec(),
    });
    assert_eq!(reply, RMessage::Write { tag: 6, count: 2 });
    assert_eq!(
        fs::read_to_string(root.join("body")).map_err(|error| Error::from(error.to_string()))?,
        "xy"
    );
    assert!(!created_qid.is_dir());

    remove_fixture(root);
    Ok(())
}

fn attach(server: &mut Server<LocalTree>) {
    let reply = server.handle(TMessage::Attach {
        tag: 1,
        fid: 1,
        afid: r9p::NOFID,
        uname: b"codex".to_vec(),
        aname: b"/".to_vec(),
    });
    assert!(matches!(reply, RMessage::Attach { .. }));
}

fn walk(server: &mut Server<LocalTree>, fid: Fid, newfid: Fid, name: &[u8]) {
    let reply = server.handle(TMessage::Walk {
        tag: 2,
        fid,
        newfid,
        wnames: vec![name.to_vec()],
    });
    assert!(matches!(reply, RMessage::Walk { .. }));
}

fn clone_fid(server: &mut Server<LocalTree>, fid: Fid, newfid: Fid) {
    let reply = server.handle(TMessage::Walk {
        tag: 2,
        fid,
        newfid,
        wnames: Vec::new(),
    });
    assert!(matches!(reply, RMessage::Walk { .. }));
}

fn open(server: &mut Server<LocalTree>, fid: Fid) {
    let reply = server.handle(TMessage::Open {
        tag: 3,
        fid,
        mode: OREAD,
    });
    assert!(matches!(reply, RMessage::Open { .. }));
}

fn fixture_root(label: &str) -> Result<PathBuf> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| Error::from(error.to_string()))?
        .as_nanos();
    let path = env::temp_dir().join(format!("r9p-fs-{label}-{}-{nanos}", process::id()));
    fs::create_dir(&path).map_err(|error| Error::from(error.to_string()))?;
    Ok(path)
}

fn remove_fixture(path: PathBuf) {
    let _ = fs::remove_dir_all(path);
}
