mod config;
mod handlers;
mod session;
mod types;
mod validation;

pub use config::ServerConfig;
pub use session::Session;
pub use types::{
    OpenFile, ReadData, ServerCompletion, ServerEvent, ServerRequest, ServerRequestKind,
};
pub use validation::{error_reply, validate_walk_names};

use crate::{
    error::{Error, Result, EPERM},
    fid::Fid,
    qid::Qid,
    stat::Stat,
};

pub trait FileTree {
    fn attach(&mut self, fid: Fid, uname: &[u8], aname: &[u8]) -> Result<Qid>;
    fn walk(&mut self, fid: Fid, newfid: Fid, start: Qid, names: &[Vec<u8>]) -> Result<Vec<Qid>>;
    fn open(&mut self, fid: Fid, qid: Qid, mode: u8) -> Result<OpenFile>;
    fn read(&mut self, fid: Fid, qid: Qid, offset: u64, count: u32) -> Result<ReadData>;
    fn stat(&mut self, qid: Qid) -> Result<Stat>;

    fn auth(&mut self, _afid: Fid, _uname: &[u8], _aname: &[u8]) -> Result<Qid> {
        Err(Error::from_static(crate::error::ENOAUTH))
    }

    fn attach_with_auth(
        &mut self,
        _fid: Fid,
        _afid: Fid,
        _uname: &[u8],
        _aname: &[u8],
    ) -> Result<Qid> {
        Err(Error::from_static(crate::error::ENOAUTH))
    }

    fn create(
        &mut self,
        _fid: Fid,
        _qid: Qid,
        _name: &[u8],
        _perm: u32,
        _mode: u8,
    ) -> Result<OpenFile> {
        Err(Error::from_static(EPERM))
    }

    fn write(&mut self, _fid: Fid, _qid: Qid, _offset: u64, _data: &[u8]) -> Result<u32> {
        Err(Error::from_static(EPERM))
    }

    fn clunk(&mut self, _fid: Fid, _qid: Qid) -> Result<()> {
        Ok(())
    }

    fn remove(&mut self, _fid: Fid, _qid: Qid) -> Result<()> {
        Err(Error::from_static(EPERM))
    }

    fn wstat(&mut self, _fid: Fid, _qid: Qid, _stat: &Stat) -> Result<()> {
        Err(Error::from_static(EPERM))
    }
}

pub struct Server<T> {
    pub(super) session: Session,
    pub(super) tree: T,
}

impl<T> Server<T> {
    pub fn new(tree: T) -> Self {
        Self::with_config(tree, ServerConfig::default())
    }

    pub fn with_config(tree: T, config: ServerConfig) -> Self {
        Self {
            session: Session::new(config),
            tree,
        }
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    pub fn tree_mut(&mut self) -> &mut T {
        &mut self.tree
    }

    pub fn into_tree(self) -> T {
        self.tree
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        error::{EBADWNAME, EFIDLIMIT, EPERM},
        fid::NOFID,
        flush::FlushOutcome,
        message::{RMessage, TMessage},
    };

    #[derive(Debug)]
    struct RootOnly {
        root: Qid,
    }

    impl RootOnly {
        fn new() -> Self {
            Self { root: Qid::dir(1) }
        }
    }

    impl FileTree for RootOnly {
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
            if names == [b".".to_vec()] {
                Ok(vec![self.root])
            } else {
                Ok(Vec::new())
            }
        }

        fn open(&mut self, _fid: Fid, qid: Qid, _mode: u8) -> Result<OpenFile> {
            Ok(OpenFile { qid, iounit: 0 })
        }

        fn read(&mut self, _fid: Fid, _qid: Qid, _offset: u64, _count: u32) -> Result<ReadData> {
            Ok(ReadData::Bytes(Vec::new()))
        }

        fn stat(&mut self, qid: Qid) -> Result<Stat> {
            Ok(Stat::new(".", qid, crate::qid::DMDIR | 0o500))
        }
    }

    #[test]
    fn tversion_resets_fids_and_requests() -> Result<()> {
        let mut server = Server::with_config(
            RootOnly::new(),
            ServerConfig {
                max_fids: 2,
                ..ServerConfig::default()
            },
        );
        let attached = server.handle(TMessage::Attach {
            tag: 1,
            fid: 1,
            afid: NOFID,
            uname: b"u".to_vec(),
            aname: Vec::new(),
        });
        assert!(matches!(attached, RMessage::Attach { .. }));
        assert_eq!(server.session().fid_count(), 1);

        let version = server.handle(TMessage::Version {
            tag: crate::message::NOTAG,
            msize: 8192,
            version: b"9P2000".to_vec(),
        });
        assert!(matches!(version, RMessage::Version { .. }));
        assert_eq!(server.session().fid_count(), 0);
        Ok(())
    }

    #[test]
    fn live_fid_cap_counts_fids_not_numeric_values() -> Result<()> {
        let mut server = Server::with_config(
            RootOnly::new(),
            ServerConfig {
                max_fids: 1,
                ..ServerConfig::default()
            },
        );
        let attached = server.handle(TMessage::Attach {
            tag: 1,
            fid: 4_000_000,
            afid: NOFID,
            uname: b"u".to_vec(),
            aname: Vec::new(),
        });
        assert!(matches!(attached, RMessage::Attach { .. }));

        let walk = server.handle(TMessage::Walk {
            tag: 2,
            fid: 4_000_000,
            newfid: 4_000_001,
            wnames: Vec::new(),
        });
        assert_eq!(
            walk,
            RMessage::Error {
                tag: 2,
                ename: EFIDLIMIT.as_bytes().to_vec()
            }
        );
        Ok(())
    }

    #[test]
    fn bad_walk_names_are_rejected_before_backend() -> Result<()> {
        let mut server = Server::new(RootOnly::new());
        let _reply = server.handle(TMessage::Attach {
            tag: 1,
            fid: 1,
            afid: NOFID,
            uname: b"u".to_vec(),
            aname: Vec::new(),
        });
        let walk = server.handle(TMessage::Walk {
            tag: 2,
            fid: 1,
            newfid: 2,
            wnames: vec![b"a/b".to_vec()],
        });
        assert_eq!(
            walk,
            RMessage::Error {
                tag: 2,
                ename: EBADWNAME.as_bytes().to_vec()
            }
        );
        Ok(())
    }

    #[test]
    fn remove_clunks_fid_even_when_backend_rejects_remove() -> Result<()> {
        let mut server = Server::new(RootOnly::new());
        let attach = server.handle(TMessage::Attach {
            tag: 1,
            fid: 1,
            afid: NOFID,
            uname: b"u".to_vec(),
            aname: Vec::new(),
        });
        assert!(matches!(attach, RMessage::Attach { .. }));
        assert!(server.session().contains_fid(1));

        let remove = server.handle(TMessage::Remove { tag: 2, fid: 1 });
        assert_eq!(
            remove,
            RMessage::Error {
                tag: 2,
                ename: EPERM.as_bytes().to_vec()
            }
        );
        assert!(!server.session().contains_fid(1));
        Ok(())
    }

    #[test]
    fn split_attach_completion_binds_fid_when_request_is_live() -> Result<()> {
        let mut server = Server::new(RootOnly::new());
        let request = match server.admit(TMessage::Attach {
            tag: 1,
            fid: 1,
            afid: NOFID,
            uname: b"u".to_vec(),
            aname: Vec::new(),
        }) {
            ServerEvent::Dispatch(request) => request,
            other => panic!("expected dispatch, got {other:?}"),
        };

        let reply = server
            .complete(request, Ok(ServerCompletion::Attach { qid: Qid::dir(1) }))
            .ok_or("live completion was dropped")?;

        assert_eq!(
            reply,
            RMessage::Attach {
                tag: 1,
                qid: Qid::dir(1)
            }
        );
        assert!(server.session().contains_fid(1));
        Ok(())
    }

    #[test]
    fn split_flush_cancels_request_and_drops_stale_completion() -> Result<()> {
        let mut server = Server::new(RootOnly::new());
        let request = match server.admit(TMessage::Attach {
            tag: 1,
            fid: 1,
            afid: NOFID,
            uname: b"u".to_vec(),
            aname: Vec::new(),
        }) {
            ServerEvent::Dispatch(request) => request,
            other => panic!("expected dispatch, got {other:?}"),
        };

        let expected_key = request.key;
        let flush = server.admit(TMessage::Flush { tag: 2, oldtag: 1 });
        assert_eq!(
            flush,
            ServerEvent::Flush {
                reply: RMessage::Flush { tag: 2 },
                outcome: FlushOutcome::Cancelled(expected_key)
            }
        );

        let late = server.complete(request, Ok(ServerCompletion::Attach { qid: Qid::dir(1) }));
        assert_eq!(late, None);
        assert!(!server.session().contains_fid(1));
        Ok(())
    }

    #[test]
    fn split_api_does_not_require_a_filetree_backend() {
        let mut server = Server::new(());
        let event = server.admit(TMessage::Version {
            tag: crate::message::NOTAG,
            msize: 8192,
            version: b"9P2000".to_vec(),
        });
        assert_eq!(
            event,
            ServerEvent::Reply(RMessage::Version {
                tag: crate::message::NOTAG,
                msize: 8192,
                version: b"9P2000".to_vec()
            })
        );
    }
}
