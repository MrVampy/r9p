use super::{
    handlers::perform_file_tree_request, serve_connection, ConnectionHandler, ConnectionStream,
    FileTree, ServerCompletion, ServerConfig, ServerRequest,
};
use crate::error::{Error, Result};
use std::sync::{Arc, Mutex};

struct FileTreeHandler<T> {
    tree: Mutex<T>,
}

impl<T> FileTreeHandler<T> {
    fn new(tree: T) -> Self {
        Self {
            tree: Mutex::new(tree),
        }
    }
}

impl<T> ConnectionHandler for FileTreeHandler<T>
where
    T: FileTree + Send + 'static,
{
    fn perform(
        &self,
        request: &ServerRequest,
        _cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<ServerCompletion> {
        let mut tree = self
            .tree
            .lock()
            .map_err(|_| Error::from_static("9P file tree poisoned"))?;
        perform_file_tree_request(&mut *tree, request)
    }

    fn reset(&self) -> Result<()> {
        self.tree
            .lock()
            .map_err(|_| Error::from_static("9P file tree poisoned"))?
            .reset()
    }
}

pub fn serve_file_tree_connection<S, T>(stream: S, config: ServerConfig, tree: T) -> Result<()>
where
    S: ConnectionStream,
    T: FileTree + Send + 'static,
{
    serve_connection(stream, config, Arc::new(FileTreeHandler::new(tree)))
}
