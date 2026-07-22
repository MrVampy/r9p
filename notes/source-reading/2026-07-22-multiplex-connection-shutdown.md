# Multiplex connection shutdown

Date: 2026-07-22.

## Question

How should a long-lived namespace subscriber interrupt its one pending 9P read
when the local presentation client detaches?

## Sources inspected

- `crates/core/src/multiplex/client.rs`
- `crates/core/src/multiplex/reader.rs`
- `crates/core/src/multiplex/mod.rs`
- `crates/session/src/request.rs`
- Vault `src/native/r9p_listener/tests/front_feed_detached.rs`
- r9wm `crates/terminal-contract/src/client.rs`
- r9wm `crates/attach/src/main.rs`
- Agents `crates/runner/src/service/tree/wait.rs`

## Findings

The multiplex client already owns transport shutdown in its final `Drop`, and
its reader thread already turns transport closure into failures for all pending
tag waiters. Consumers could flush an individually tracked request, but a
presentation client that owns a dedicated subscription connection needs to
cancel the entire connection at detach. Keeping the subscription fid open also
preserves the server's per-fid cursor and avoids polling by repeated walk, open,
flush, clunk, and reconnect cycles.

## Effect

`MultiplexedClient::shutdown` exposes the existing whole-connection lifecycle
operation. It remains transport-generic and backend-neutral. R9wm can now block
on one ordinary subscription read and wake its observer during local detach by
shutting down only that dedicated connection.

## Open questions

None for the terminal attacher. Callers that multiplex unrelated work on one
connection should continue to use tag-specific flush rather than whole-client
shutdown.
