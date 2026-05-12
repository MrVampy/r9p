use r9p::{
    client::{Client, ClientResponse, Completion},
    error::{Result, EPERM},
    fid::NOFID,
    message::{RMessage, TMessage},
    qid::{Qid, DMDIR, QTDIR},
    server::{FileTree, OpenFile, ReadData, Server},
    stat::Stat,
};

#[derive(Debug)]
struct MemoryTree {
    root: Qid,
    hello: Qid,
    nested: Qid,
    nested_note: Qid,
}

impl MemoryTree {
    fn new() -> Self {
        Self {
            root: Qid::dir(1),
            hello: Qid::file(2),
            nested: Qid::dir(3),
            nested_note: Qid::file(4),
        }
    }
}

impl FileTree for MemoryTree {
    fn attach(&mut self, _fid: r9p::Fid, _uname: &[u8], _aname: &[u8]) -> Result<Qid> {
        Ok(self.root)
    }

    fn walk(
        &mut self,
        _fid: r9p::Fid,
        _newfid: r9p::Fid,
        start: Qid,
        names: &[Vec<u8>],
    ) -> Result<Vec<Qid>> {
        let mut current = start;
        let mut qids = Vec::new();
        for name in names {
            current = match (current.path, name.as_slice()) {
                (_, b".") => current,
                (_, b"..") => self.root,
                (1, b"hello.txt") => self.hello,
                (1, b"nested") => self.nested,
                (3, b"note.txt") => self.nested_note,
                _ => break,
            };
            qids.push(current);
        }
        Ok(qids)
    }

    fn open(&mut self, _fid: r9p::Fid, qid: Qid, _mode: u8) -> Result<OpenFile> {
        Ok(OpenFile { qid, iounit: 0 })
    }

    fn read(&mut self, _fid: r9p::Fid, qid: Qid, offset: u64, count: u32) -> Result<ReadData> {
        if qid.qtype == QTDIR {
            let stats = if qid == self.root {
                vec![
                    self.stat(self.hello)?,
                    self.stat(self.nested)?,
                    self.stat(self.nested_note)?,
                ]
            } else {
                vec![self.stat(self.nested_note)?]
            };
            return Ok(ReadData::Directory(stats));
        }

        let data = match qid.path {
            2 => b"hello from r9p\n".as_slice(),
            4 => b"nested note\n".as_slice(),
            _ => b"".as_slice(),
        };
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(data.len());
        let end = start
            .saturating_add(usize::try_from(count).unwrap_or(usize::MAX))
            .min(data.len());
        Ok(ReadData::Bytes(data[start..end].to_vec()))
    }

    fn stat(&mut self, qid: Qid) -> Result<Stat> {
        Ok(match qid.path {
            1 => Stat::new(".", qid, DMDIR | 0o500),
            2 => Stat::new("hello.txt", qid, 0o400),
            3 => Stat::new("nested", qid, DMDIR | 0o500),
            4 => Stat::new("note.txt", qid, 0o400),
            _ => Stat::new("missing", qid, 0),
        })
    }
}

#[test]
fn memory_tree_walk_open_read_with_in_tree_client() -> Result<()> {
    let mut server = Server::new(MemoryTree::new());
    let mut client = Client::new();

    let version = client.version_request(8192);
    let version_reply = server.handle(version);
    assert!(matches!(
        client.receive(version_reply)?,
        ClientResponse::Completion {
            completion: Completion::Version { .. },
            ..
        }
    ));

    let attach = client.attach("glenda", "")?;
    let root_fid = attach.fid.ok_or("attach missing fid")?;
    let attach_reply = server.handle(attach.message);
    assert!(matches!(attach_reply, RMessage::Attach { .. }));
    assert!(matches!(
        client.receive(attach_reply)?,
        ClientResponse::Completion {
            completion: Completion::Attach { .. },
            ..
        }
    ));

    let walk = client.walk(root_fid, vec![b"hello.txt".to_vec()])?;
    let hello_fid = walk.fid.ok_or("walk missing fid")?;
    assert!(matches!(
        client.receive(server.handle(walk.message))?,
        ClientResponse::Completion {
            completion: Completion::Walk { .. },
            ..
        }
    ));

    let open = client.open(hello_fid, 0)?;
    assert!(matches!(
        client.receive(server.handle(open.message))?,
        ClientResponse::Completion {
            completion: Completion::Open { .. },
            ..
        }
    ));

    let read = client.read(hello_fid, 0, 100)?;
    let response = client.receive(server.handle(read.message))?;
    match response {
        ClientResponse::Completion {
            completion: Completion::Read { data },
            ..
        } => assert_eq!(data, b"hello from r9p\n".to_vec()),
        other => panic!("unexpected read response: {other:?}"),
    }
    Ok(())
}

#[test]
fn memory_tree_is_not_acme_or_racme_core() -> Result<()> {
    let mut server = Server::new(MemoryTree::new());
    let reply = server.handle(TMessage::Attach {
        tag: 1,
        fid: 1,
        afid: NOFID,
        uname: b"glenda".to_vec(),
        aname: Vec::new(),
    });
    assert!(matches!(reply, RMessage::Attach { .. }));

    let create = server.handle(TMessage::Create {
        tag: 2,
        fid: 1,
        name: b"new".to_vec(),
        perm: 0o600,
        mode: 1,
    });
    assert_eq!(
        create,
        RMessage::Error {
            tag: 2,
            ename: EPERM.as_bytes().to_vec()
        }
    );
    Ok(())
}
