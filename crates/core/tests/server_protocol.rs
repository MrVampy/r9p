use r9p::{
    codec::{self, Variant},
    error::{
        Error, Result, EBADDIROFFSET, EBADMODE, EFIDBUSY, EFIDINUSE, EFIDNOTOPEN, EFIDOPEN,
        EWSTATATIME, EWSTATDEV, EWSTATDIRLENGTH, EWSTATDMDIR, EWSTATMUID, EWSTATQID, EWSTATTYPE,
        EWSTATUID,
    },
    fid::{Fid, NOFID},
    message::{RMessage, TMessage, NOTAG},
    qid::{Qid, DMDIR},
    server::{
        FileTree, OpenFile, ReadData, Server, ServerCompletion, ServerConfig, ServerEvent,
        ServerRequest,
    },
    stat::Stat,
    ORCLOSE, ORDWR, OREAD, OWRITE,
};

#[derive(Debug)]
struct ProtocolTree {
    root: Qid,
    file: Qid,
    write_count: Option<u32>,
    wstat_count: usize,
    iounit: u32,
}

impl ProtocolTree {
    fn new() -> Self {
        Self {
            root: Qid::dir(1),
            file: Qid::file(2),
            write_count: None,
            wstat_count: 0,
            iounit: 0,
        }
    }
}

impl FileTree for ProtocolTree {
    fn attach(&mut self, _fid: Fid, _uname: &[u8], _aname: &[u8]) -> Result<Qid> {
        Ok(self.root)
    }

    fn walk(
        &mut self,
        _fid: Fid,
        _newfid: Fid,
        _start: Qid,
        names: &[Vec<u8>],
    ) -> Result<Vec<Qid>> {
        Ok(names.iter().map(|_| self.file).collect())
    }

    fn open(&mut self, _fid: Fid, qid: Qid, _mode: u8) -> Result<OpenFile> {
        Ok(OpenFile {
            qid,
            iounit: self.iounit,
        })
    }

    fn read(&mut self, _fid: Fid, qid: Qid, _offset: u64, _count: u32) -> Result<ReadData> {
        if qid.is_dir() {
            Ok(ReadData::Directory(vec![
                Stat::new("first", Qid::file(11), 0o444),
                Stat::new("second", Qid::file(12), 0o444),
            ]))
        } else {
            Ok(ReadData::Bytes(b"value".to_vec()))
        }
    }

    fn write(&mut self, _fid: Fid, _qid: Qid, _offset: u64, data: &[u8]) -> Result<u32> {
        self.write_count.map(Ok).unwrap_or_else(|| {
            u32::try_from(data.len()).map_err(|_| Error::from("write too large"))
        })
    }

    fn stat(&mut self, qid: Qid) -> Result<Stat> {
        Ok(Stat::new(
            if qid.is_dir() { "." } else { "value" },
            qid,
            if qid.is_dir() { DMDIR | 0o555 } else { 0o444 },
        ))
    }

    fn wstat(&mut self, _fid: Fid, _qid: Qid, _stat: &Stat) -> Result<()> {
        self.wstat_count += 1;
        Ok(())
    }
}

fn negotiate<T>(server: &mut Server<T>) {
    assert!(matches!(
        server.admit(TMessage::Version {
            tag: NOTAG,
            msize: 8192,
            version: b"9P2000".to_vec(),
        }),
        ServerEvent::Reply(RMessage::Version {
            version,
            ..
        }) if version == b"9P2000"
    ));
}

fn attach(server: &mut Server<ProtocolTree>, fid: Fid) {
    assert!(matches!(
        server.handle(TMessage::Attach {
            tag: 1,
            fid,
            afid: NOFID,
            uname: b"glenda".to_vec(),
            aname: Vec::new(),
        }),
        RMessage::Attach { .. }
    ));
}

fn walk_file(server: &mut Server<ProtocolTree>, fid: Fid, newfid: Fid) {
    assert!(matches!(
        server.handle(TMessage::Walk {
            tag: 2,
            fid,
            newfid,
            wnames: vec![b"value".to_vec()],
        }),
        RMessage::Walk { .. }
    ));
}

fn assert_error(reply: RMessage, expected: &str) {
    match reply {
        RMessage::Error { ename, .. } => assert_eq!(ename, expected.as_bytes()),
        other => panic!("expected protocol error {expected:?}, got {other:?}"),
    }
}

fn dispatch(event: ServerEvent) -> ServerRequest {
    match event {
        ServerEvent::Dispatch(request) => request,
        other => panic!("expected backend dispatch, got {other:?}"),
    }
}

#[test]
fn version_is_required_and_unknown_versions_get_rversion_unknown() {
    let mut server = Server::new(ProtocolTree::new());
    assert_error(
        server.handle(TMessage::Attach {
            tag: 1,
            fid: 1,
            afid: NOFID,
            uname: Vec::new(),
            aname: Vec::new(),
        }),
        "version not negotiated",
    );

    assert_eq!(
        server.handle(TMessage::Version {
            tag: NOTAG,
            msize: 8192,
            version: b"not-9p".to_vec(),
        }),
        RMessage::Version {
            tag: NOTAG,
            msize: codec::MIN_MSIZE,
            version: b"unknown".to_vec(),
        }
    );
    assert!(!server.session().is_negotiated());

    negotiate(&mut server);
    assert_error(
        server.handle(TMessage::Version {
            tag: NOTAG,
            msize: codec::MIN_MSIZE - 1,
            version: b"9P2000".to_vec(),
        }),
        r9p::error::EBADMSIZE,
    );
    assert!(!server.session().is_negotiated());
}

#[test]
fn plain_version_accepts_period_extensions_not_prefix_collisions() {
    assert_eq!(Variant::Plain.accept(b"9P2000.u"), Some(Variant::Plain));
    assert_eq!(
        Variant::Plain.accept(b"9P2000.L.cache"),
        Some(Variant::Plain)
    );
    assert_eq!(Variant::Plain.accept(b"9P2000garbage"), None);
}

#[test]
fn symlink_extension_is_negotiated_explicitly_and_can_downgrade() {
    assert_eq!(
        Variant::R9pSymlink.accept(b"9P2000.r9p-symlink"),
        Some(Variant::R9pSymlink)
    );
    assert_eq!(Variant::R9pSymlink.accept(b"9P2000"), Some(Variant::Plain));
    assert_eq!(
        Variant::R9pSymlink.accept_response(b"9P2000"),
        Some(Variant::Plain)
    );
    assert_eq!(Variant::Plain.accept_response(b"9P2000.r9p-symlink"), None);

    let mut server = Server::with_config(
        ProtocolTree::new(),
        ServerConfig {
            variant: Variant::R9pSymlink,
            ..ServerConfig::default()
        },
    );
    assert_eq!(
        server.handle(TMessage::Version {
            tag: NOTAG,
            msize: 8192,
            version: b"9P2000.r9p-symlink".to_vec(),
        }),
        RMessage::Version {
            tag: NOTAG,
            msize: 8192,
            version: b"9P2000.r9p-symlink".to_vec(),
        }
    );
    assert_eq!(server.session().variant(), Some(Variant::R9pSymlink));
}

#[test]
fn immutable_wstat_fields_are_rejected_before_backend_dispatch() {
    let mut server = Server::new(ProtocolTree::new());
    negotiate(&mut server);
    attach(&mut server, 1);
    walk_file(&mut server, 1, 2);

    let mut cases: Vec<(Stat, &str)> = Vec::new();
    let mut stat = Stat::null_wstat();
    stat.type_ = 0;
    cases.push((stat, EWSTATTYPE));
    let mut stat = Stat::null_wstat();
    stat.dev = 0;
    cases.push((stat, EWSTATDEV));
    let mut stat = Stat::null_wstat();
    stat.qid = Qid::file(2);
    cases.push((stat, EWSTATQID));
    let mut stat = Stat::null_wstat();
    stat.atime = 0;
    cases.push((stat, EWSTATATIME));
    let mut stat = Stat::null_wstat();
    stat.uid = b"glenda".to_vec();
    cases.push((stat, EWSTATUID));
    let mut stat = Stat::null_wstat();
    stat.muid = b"glenda".to_vec();
    cases.push((stat, EWSTATMUID));
    let mut stat = Stat::null_wstat();
    stat.mode = DMDIR | 0o755;
    cases.push((stat, EWSTATDMDIR));

    for (index, (stat, expected)) in cases.into_iter().enumerate() {
        assert_error(
            server.handle(TMessage::Wstat {
                tag: 10 + u16::try_from(index).unwrap_or(0),
                fid: 2,
                stat,
            }),
            expected,
        );
    }
    assert_eq!(server.tree_mut().wstat_count, 0);

    let mut stat = Stat::null_wstat();
    stat.name = b"renamed".to_vec();
    assert!(matches!(
        server.handle(TMessage::Wstat {
            tag: 20,
            fid: 2,
            stat,
        }),
        RMessage::Wstat { tag: 20 }
    ));
    assert_eq!(server.tree_mut().wstat_count, 1);
}

#[test]
fn nonzero_directory_length_is_rejected_before_backend_dispatch() {
    let mut server = Server::new(ProtocolTree::new());
    negotiate(&mut server);
    attach(&mut server, 1);
    let mut stat = Stat::null_wstat();
    stat.length = 1;
    assert_error(
        server.handle(TMessage::Wstat {
            tag: 2,
            fid: 1,
            stat,
        }),
        EWSTATDIRLENGTH,
    );
    assert_eq!(server.tree_mut().wstat_count, 0);
}

#[test]
fn walk_names_use_the_wire_string_bound_not_an_accidental_byte_bound() {
    let mut server = Server::new(ProtocolTree::new());
    negotiate(&mut server);
    attach(&mut server, 1);
    let component = vec![b'x'; 256];
    assert!(matches!(
        server.handle(TMessage::Walk {
            tag: 2,
            fid: 1,
            newfid: 2,
            wnames: vec![component],
        }),
        RMessage::Walk { qids, .. } if qids == vec![Qid::file(2)]
    ));
}

#[test]
fn core_enforces_open_lifecycle_and_access_mode() {
    let mut server = Server::new(ProtocolTree::new());
    negotiate(&mut server);
    attach(&mut server, 1);
    walk_file(&mut server, 1, 2);

    assert_error(
        server.handle(TMessage::Read {
            tag: 3,
            fid: 2,
            offset: 0,
            count: 32,
        }),
        EFIDNOTOPEN,
    );
    assert!(matches!(
        server.handle(TMessage::Open {
            tag: 4,
            fid: 2,
            mode: OREAD,
        }),
        RMessage::Open { .. }
    ));
    assert_error(
        server.handle(TMessage::Open {
            tag: 5,
            fid: 2,
            mode: OREAD,
        }),
        EFIDOPEN,
    );
    assert_error(
        server.handle(TMessage::Walk {
            tag: 6,
            fid: 2,
            newfid: 3,
            wnames: Vec::new(),
        }),
        EFIDOPEN,
    );
    assert_error(
        server.handle(TMessage::Write {
            tag: 7,
            fid: 2,
            offset: 0,
            data: b"no".to_vec(),
        }),
        EFIDNOTOPEN,
    );
}

#[test]
fn auth_fids_are_open_read_write_without_topen() -> Result<()> {
    let mut server = Server::new(());
    negotiate(&mut server);
    let auth = dispatch(server.admit(TMessage::Auth {
        tag: 1,
        afid: 9,
        uname: b"glenda".to_vec(),
        aname: Vec::new(),
    }));
    assert_eq!(
        server.complete(auth, Ok(ServerCompletion::Auth { qid: Qid::auth(9) }),),
        Some(RMessage::Auth {
            tag: 1,
            aqid: Qid::auth(9),
        })
    );
    assert_eq!(server.session().fid(9)?.open_mode(), Some(ORDWR));

    match server.admit(TMessage::Open {
        tag: 2,
        fid: 9,
        mode: ORDWR,
    }) {
        ServerEvent::Reply(reply) => assert_error(reply, EFIDOPEN),
        other => panic!("auth fid admitted Topen: {other:?}"),
    }
    let write = dispatch(server.admit(TMessage::Write {
        tag: 3,
        fid: 9,
        offset: 0,
        data: b"challenge".to_vec(),
    }));
    assert_eq!(
        server.complete(write, Ok(ServerCompletion::Write { count: 9 })),
        Some(RMessage::Write { tag: 3, count: 9 })
    );
    Ok(())
}

#[test]
fn invalid_modes_and_directory_writes_are_rejected_before_backend() {
    let mut server = Server::new(ProtocolTree::new());
    negotiate(&mut server);
    attach(&mut server, 1);
    assert_error(
        server.handle(TMessage::Open {
            tag: 2,
            fid: 1,
            mode: OWRITE,
        }),
        EBADMODE,
    );
    assert_error(
        server.handle(TMessage::Open {
            tag: 3,
            fid: 1,
            mode: 0x80,
        }),
        EBADMODE,
    );
    assert!(matches!(
        server.handle(TMessage::Open {
            tag: 4,
            fid: 1,
            mode: OREAD | ORCLOSE,
        }),
        RMessage::Open { .. }
    ));
}

#[test]
fn directory_reads_only_continue_at_the_previous_reply_boundary() -> Result<()> {
    let first = Stat::new("first", Qid::file(11), 0o444).encode()?;
    let first_count = u32::try_from(first.len()).map_err(|_| Error::from("test stat too large"))?;
    let next_offset = u64::from(first_count);
    let mut server = Server::new(ProtocolTree::new());
    negotiate(&mut server);
    attach(&mut server, 1);
    assert!(matches!(
        server.handle(TMessage::Open {
            tag: 2,
            fid: 1,
            mode: OREAD,
        }),
        RMessage::Open { .. }
    ));
    assert!(matches!(
        server.handle(TMessage::Read {
            tag: 3,
            fid: 1,
            offset: 0,
            count: first_count,
        }),
        RMessage::Read { data, .. } if data == first
    ));
    assert_error(
        server.handle(TMessage::Read {
            tag: 4,
            fid: 1,
            offset: 1,
            count: 8192,
        }),
        EBADDIROFFSET,
    );
    assert!(matches!(
        server.handle(TMessage::Read {
            tag: 5,
            fid: 1,
            offset: next_offset,
            count: 8192,
        }),
        RMessage::Read { data, .. } if !data.is_empty()
    ));
    Ok(())
}

#[test]
fn split_fid_transitions_are_reserved_and_flush_rolls_them_back() -> Result<()> {
    let mut server = Server::new(());
    negotiate(&mut server);
    let attach = dispatch(server.admit(TMessage::Attach {
        tag: 1,
        fid: 9,
        afid: NOFID,
        uname: Vec::new(),
        aname: Vec::new(),
    }));
    let _reply = server
        .complete(attach, Ok(ServerCompletion::Attach { qid: Qid::file(9) }))
        .ok_or("attach completion dropped")?;

    let open = dispatch(server.admit(TMessage::Open {
        tag: 2,
        fid: 9,
        mode: OREAD,
    }));
    match server.admit(TMessage::Stat { tag: 3, fid: 9 }) {
        ServerEvent::Reply(reply) => assert_error(reply, EFIDBUSY),
        other => panic!("reserved fid admitted a second operation: {other:?}"),
    }
    assert!(matches!(
        server.admit(TMessage::Flush { tag: 4, oldtag: 2 }),
        ServerEvent::Flush { .. }
    ));
    assert_eq!(
        server.complete(
            open,
            Ok(ServerCompletion::Open(OpenFile {
                qid: Qid::file(9),
                iounit: 0,
            })),
        ),
        None
    );
    assert_eq!(server.session().fid(9)?.open_mode(), None);
    assert!(matches!(
        server.admit(TMessage::Open {
            tag: 5,
            fid: 9,
            mode: OREAD,
        }),
        ServerEvent::Dispatch(_)
    ));
    Ok(())
}

#[test]
fn split_clone_walks_can_share_a_source_fid() -> Result<()> {
    let mut server = Server::new(());
    negotiate(&mut server);
    let attach = dispatch(server.admit(TMessage::Attach {
        tag: 1,
        fid: 1,
        afid: NOFID,
        uname: Vec::new(),
        aname: Vec::new(),
    }));
    let _reply = server
        .complete(attach, Ok(ServerCompletion::Attach { qid: Qid::dir(1) }))
        .ok_or("attach completion dropped")?;

    let first = dispatch(server.admit(TMessage::Walk {
        tag: 2,
        fid: 1,
        newfid: 2,
        wnames: vec![b"first".to_vec()],
    }));
    let second = dispatch(server.admit(TMessage::Walk {
        tag: 3,
        fid: 1,
        newfid: 3,
        wnames: vec![b"second".to_vec()],
    }));

    match server.admit(TMessage::Clunk { tag: 4, fid: 1 }) {
        ServerEvent::Reply(reply) => assert_error(reply, EFIDBUSY),
        other => panic!("shared walk source admitted clunk: {other:?}"),
    }
    let stat = dispatch(server.admit(TMessage::Stat { tag: 5, fid: 1 }));
    assert!(matches!(
        server.complete(
            stat,
            Ok(ServerCompletion::Stat {
                stat: Stat::new(".", Qid::dir(1), DMDIR | 0o555),
            }),
        ),
        Some(RMessage::Stat { tag: 5, .. })
    ));

    assert!(matches!(
        server.complete(
            first,
            Ok(ServerCompletion::Walk {
                qids: vec![Qid::file(2)],
            }),
        ),
        Some(RMessage::Walk { tag: 2, .. })
    ));
    match server.admit(TMessage::Open {
        tag: 6,
        fid: 1,
        mode: OREAD,
    }) {
        ServerEvent::Reply(reply) => assert_error(reply, EFIDBUSY),
        other => panic!("shared walk source admitted open: {other:?}"),
    }
    assert!(matches!(
        server.complete(
            second,
            Ok(ServerCompletion::Walk {
                qids: vec![Qid::file(3)],
            }),
        ),
        Some(RMessage::Walk { tag: 3, .. })
    ));
    assert_eq!(server.session().fid(2)?.qid, Qid::file(2));
    assert_eq!(server.session().fid(3)?.qid, Qid::file(3));
    let clunk = dispatch(server.admit(TMessage::Clunk { tag: 7, fid: 1 }));
    assert_eq!(
        server.complete(clunk, Ok(ServerCompletion::Clunk)),
        Some(RMessage::Clunk { tag: 7 })
    );
    Ok(())
}

#[test]
fn clunk_and_remove_reserve_the_retired_fid_until_completion() -> Result<()> {
    let mut server = Server::new(());
    negotiate(&mut server);
    let attach = dispatch(server.admit(TMessage::Attach {
        tag: 1,
        fid: 9,
        afid: NOFID,
        uname: Vec::new(),
        aname: Vec::new(),
    }));
    let _reply = server
        .complete(attach, Ok(ServerCompletion::Attach { qid: Qid::file(9) }))
        .ok_or("attach completion dropped")?;

    let clunk = dispatch(server.admit(TMessage::Clunk { tag: 2, fid: 9 }));
    match server.admit(TMessage::Attach {
        tag: 3,
        fid: 9,
        afid: NOFID,
        uname: Vec::new(),
        aname: Vec::new(),
    }) {
        ServerEvent::Reply(reply) => assert_error(reply, EFIDINUSE),
        other => panic!("retired clunk fid was reused before completion: {other:?}"),
    }
    assert_eq!(
        server.complete(clunk, Ok(ServerCompletion::Clunk)),
        Some(RMessage::Clunk { tag: 2 })
    );

    let attach = dispatch(server.admit(TMessage::Attach {
        tag: 4,
        fid: 9,
        afid: NOFID,
        uname: Vec::new(),
        aname: Vec::new(),
    }));
    let _reply = server
        .complete(attach, Ok(ServerCompletion::Attach { qid: Qid::file(9) }))
        .ok_or("replacement attach completion dropped")?;
    let remove = dispatch(server.admit(TMessage::Remove { tag: 5, fid: 9 }));
    match server.admit(TMessage::Attach {
        tag: 6,
        fid: 9,
        afid: NOFID,
        uname: Vec::new(),
        aname: Vec::new(),
    }) {
        ServerEvent::Reply(reply) => assert_error(reply, EFIDINUSE),
        other => panic!("retired remove fid was reused before completion: {other:?}"),
    }
    assert_eq!(
        server.complete(remove, Ok(ServerCompletion::Remove)),
        Some(RMessage::Remove { tag: 5 })
    );
    assert!(matches!(
        server.admit(TMessage::Attach {
            tag: 7,
            fid: 9,
            afid: NOFID,
            uname: Vec::new(),
            aname: Vec::new(),
        }),
        ServerEvent::Dispatch(_)
    ));
    Ok(())
}

#[test]
fn partial_walk_reports_qids_without_binding_newfid() -> Result<()> {
    let mut server = Server::new(());
    negotiate(&mut server);
    let attach = dispatch(server.admit(TMessage::Attach {
        tag: 1,
        fid: 1,
        afid: NOFID,
        uname: Vec::new(),
        aname: Vec::new(),
    }));
    let _reply = server
        .complete(attach, Ok(ServerCompletion::Attach { qid: Qid::dir(1) }))
        .ok_or("attach completion dropped")?;
    let walk = dispatch(server.admit(TMessage::Walk {
        tag: 2,
        fid: 1,
        newfid: 2,
        wnames: vec![b"present".to_vec(), b"missing".to_vec()],
    }));

    assert_eq!(
        server.complete(
            walk,
            Ok(ServerCompletion::Walk {
                qids: vec![Qid::dir(2)],
            }),
        ),
        Some(RMessage::Walk {
            tag: 2,
            qids: vec![Qid::dir(2)],
        })
    );
    assert_eq!(server.session().fid(1)?.qid, Qid::dir(1));
    let error = server
        .session()
        .fid(2)
        .expect_err("partial walk bound newfid");
    assert_eq!(error.message(), r9p::error::EBADFID.as_bytes());
    Ok(())
}

#[test]
fn backend_write_counts_and_iounits_are_bounded_by_the_request() {
    let mut tree = ProtocolTree::new();
    tree.write_count = Some(4);
    tree.iounit = u32::MAX;
    let mut server = Server::new(tree);
    negotiate(&mut server);
    attach(&mut server, 1);
    walk_file(&mut server, 1, 2);
    match server.handle(TMessage::Open {
        tag: 3,
        fid: 2,
        mode: OWRITE,
    }) {
        RMessage::Open { iounit, .. } => {
            assert_eq!(iounit, codec::max_iounit(server.session().msize()));
        }
        other => panic!("expected open reply, got {other:?}"),
    }
    assert_error(
        server.handle(TMessage::Write {
            tag: 4,
            fid: 2,
            offset: 0,
            data: b"abc".to_vec(),
        }),
        "write completion exceeds request count",
    );
}
