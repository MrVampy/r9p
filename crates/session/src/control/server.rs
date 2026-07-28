use super::{tree::ControlTree, ControlConfig};
use crate::{feed::FeedState, ClientSession, Error, NamespaceCache, Result, SessionEpoch};
use r9p::server::{serve_file_tree_connection, ConnectionStream, ServerConfig};

const CONTROL_MSIZE: u32 = 65_536;

pub(super) fn serve_control_connection<S>(
    stream: S,
    client: ClientSession,
    config: ControlConfig,
    feed_state: FeedState,
    cache: NamespaceCache,
    session_epoch: SessionEpoch,
) -> Result<()>
where
    S: ConnectionStream,
{
    serve_file_tree_connection(
        stream,
        ServerConfig {
            default_msize: CONTROL_MSIZE,
            max_msize: CONTROL_MSIZE,
            ..ServerConfig::default()
        },
        ControlTree::new(client, config, feed_state, cache, session_epoch),
    )
    .map_err(|error| Error::new(libc::EPROTO, error.display_lossy().to_string()))
}
