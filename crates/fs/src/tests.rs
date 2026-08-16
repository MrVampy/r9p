use super::*;
use r9p::{
    blocking::{OREAD, OTRUNC, OWRITE},
    codec::Variant,
    message::{RMessage, TMessage},
    server::{Server, ServerConfig},
    stat::{decode_dir_entries, Stat},
};
use std::{
    env, fs,
    os::unix::{ffi::OsStrExt, fs::symlink},
    process,
    time::{Duration, SystemTime, UNIX_EPOCH},
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
fn plain_dialect_does_not_expose_symlink_metadata() -> Result<()> {
    let root = fixture_root("plain-symlink")?;
    symlink("target", root.join("link")).map_err(|error| Error::from(error.to_string()))?;
    let mut server = Server::new(LocalTree::open(&root)?);
    attach(&mut server);
    let reply = server.handle(TMessage::Walk {
        tag: 2,
        fid: 1,
        newfid: 2,
        wnames: vec![b"link".to_vec()],
    });
    assert_eq!(
        reply,
        RMessage::Error {
            tag: 2,
            ename: r9p::error::ESYMLINKDIALECT.as_bytes().to_vec(),
        }
    );
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

    let mut server = Server::with_config(
        LocalTree::open(&root)?,
        ServerConfig {
            variant: Variant::R,
            ..ServerConfig::default()
        },
    );
    attach_with_variant(&mut server, Variant::R);
    let reply = server.handle(TMessage::Walk {
        tag: 2,
        fid: 1,
        newfid: 2,
        wnames: vec![b"secret-link".to_vec()],
    });
    assert!(matches!(reply, RMessage::Walk { .. }));
    let reply = server.handle(TMessage::Open {
        tag: 3,
        fid: 2,
        mode: OREAD,
    });
    assert!(matches!(reply, RMessage::Open { .. }));
    let reply = server.handle(TMessage::Read {
        tag: 4,
        fid: 2,
        offset: 0,
        count: 8192,
    });
    assert_eq!(
        reply,
        RMessage::Read {
            tag: 4,
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
    fs::OpenOptions::new()
        .write(true)
        .open(root.join("body"))
        .and_then(|file| {
            file.set_times(fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1)))
        })
        .map_err(|error| Error::from(error.to_string()))?;

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

#[test]
fn writable_wstat_rejects_non_atomic_rename_and_truncate_without_mutation() -> Result<()> {
    let root = fixture_root("wstat-atomic")?;
    fs::write(root.join("body"), b"abcdef").map_err(|error| Error::from(error.to_string()))?;
    let mut server = Server::new(LocalTree::open_with_config(
        &root,
        LocalTreeConfig { writable: true },
    )?);
    attach(&mut server);
    walk(&mut server, 1, 2, b"body");

    let mut stat = Stat::null_wstat();
    stat.name = b"renamed".to_vec();
    stat.length = 2;
    let reply = server.handle(TMessage::Wstat {
        tag: 3,
        fid: 2,
        stat,
    });
    assert!(matches!(reply, RMessage::Error { .. }));
    assert_eq!(
        fs::read(root.join("body")).map_err(|error| Error::from(error.to_string()))?,
        b"abcdef"
    );
    assert!(!root.join("renamed").exists());

    remove_fixture(root);
    Ok(())
}

#[test]
fn writable_wstat_rename_never_replaces_an_existing_name() -> Result<()> {
    let root = fixture_root("wstat-no-replace")?;
    fs::write(root.join("body"), b"source").map_err(|error| Error::from(error.to_string()))?;
    fs::write(root.join("target"), b"target").map_err(|error| Error::from(error.to_string()))?;
    let mut server = Server::new(LocalTree::open_with_config(
        &root,
        LocalTreeConfig { writable: true },
    )?);
    attach(&mut server);
    walk(&mut server, 1, 2, b"body");

    let mut stat = Stat::null_wstat();
    stat.name = b"target".to_vec();
    let reply = server.handle(TMessage::Wstat {
        tag: 3,
        fid: 2,
        stat,
    });
    assert!(matches!(reply, RMessage::Error { .. }));
    assert_eq!(
        fs::read(root.join("body")).map_err(|error| Error::from(error.to_string()))?,
        b"source"
    );
    assert_eq!(
        fs::read(root.join("target")).map_err(|error| Error::from(error.to_string()))?,
        b"target"
    );

    remove_fixture(root);
    Ok(())
}

#[test]
fn writable_wstat_applies_supported_single_mutations() -> Result<()> {
    let root = fixture_root("wstat-single")?;
    fs::write(root.join("body"), b"abcdef").map_err(|error| Error::from(error.to_string()))?;
    let mut server = Server::new(LocalTree::open_with_config(
        &root,
        LocalTreeConfig { writable: true },
    )?);
    attach(&mut server);
    walk(&mut server, 1, 2, b"body");

    let mut stat = Stat::null_wstat();
    stat.length = 3;
    assert_eq!(
        server.handle(TMessage::Wstat {
            tag: 3,
            fid: 2,
            stat,
        }),
        RMessage::Wstat { tag: 3 }
    );
    assert_eq!(
        fs::read(root.join("body")).map_err(|error| Error::from(error.to_string()))?,
        b"abc"
    );

    let mut stat = Stat::null_wstat();
    stat.name = b"renamed".to_vec();
    assert_eq!(
        server.handle(TMessage::Wstat {
            tag: 4,
            fid: 2,
            stat,
        }),
        RMessage::Wstat { tag: 4 }
    );
    assert!(!root.join("body").exists());
    assert_eq!(
        fs::read(root.join("renamed")).map_err(|error| Error::from(error.to_string()))?,
        b"abc"
    );

    remove_fixture(root);
    Ok(())
}

#[test]
fn writable_r_dialect_rename_at_moves_and_replaces_in_one_owner_call() -> Result<()> {
    let root = fixture_root("rename-at")?;
    fs::create_dir(root.join("old")).map_err(|error| Error::from(error.to_string()))?;
    fs::create_dir(root.join("new")).map_err(|error| Error::from(error.to_string()))?;
    fs::write(root.join("old/source"), b"source")
        .map_err(|error| Error::from(error.to_string()))?;
    fs::write(root.join("new/target"), b"replaced")
        .map_err(|error| Error::from(error.to_string()))?;

    let mut server = Server::with_config(
        LocalTree::open_with_config(&root, LocalTreeConfig { writable: true })?,
        ServerConfig {
            variant: Variant::R,
            ..ServerConfig::default()
        },
    );
    attach_with_variant(&mut server, Variant::R);
    walk(&mut server, 1, 2, b"old");
    walk(&mut server, 1, 3, b"new");
    walk(&mut server, 2, 4, b"source");
    let before = match server.handle(TMessage::Stat { tag: 5, fid: 4 }) {
        RMessage::Stat { stat, .. } => stat,
        other => return Err(Error::from(format!("unexpected source stat: {other:?}"))),
    };

    assert_eq!(
        server.handle(TMessage::RenameAt {
            tag: 6,
            olddirfid: 2,
            oldname: b"source".to_vec(),
            newdirfid: 3,
            newname: b"target".to_vec(),
        }),
        RMessage::RenameAt { tag: 6 }
    );
    assert!(!root.join("old/source").exists());
    assert_eq!(
        fs::read(root.join("new/target")).map_err(|error| Error::from(error.to_string()))?,
        b"source"
    );
    let after = match server.handle(TMessage::Stat { tag: 7, fid: 4 }) {
        RMessage::Stat { stat, .. } => stat,
        other => return Err(Error::from(format!("unexpected renamed stat: {other:?}"))),
    };
    assert_eq!(after.qid.path, before.qid.path);
    assert_eq!(after.name, b"target");

    remove_fixture(root);
    Ok(())
}

fn attach(server: &mut Server<LocalTree>) {
    attach_with_variant(server, Variant::Plain);
}

fn attach_with_variant(server: &mut Server<LocalTree>, variant: Variant) {
    let version = server.handle(TMessage::Version {
        tag: r9p::NOTAG,
        msize: 8192,
        version: variant.wire_name().to_vec(),
    });
    assert!(matches!(version, RMessage::Version { .. }));
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
