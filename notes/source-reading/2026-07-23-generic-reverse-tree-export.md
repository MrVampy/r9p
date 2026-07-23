# Generic Reverse Tree Export

Date: 2026-07-23

## Question

Can an application publish a non-filesystem 9P tree over the existing
authenticated reverse-connect transport without duplicating the reverse
connection, authentication, pooling, reconnect, or server-session machinery?

## Sources Inspected

- `crates/reverse/src/export.rs`
- `crates/reverse/src/broker.rs`
- `crates/reverse/src/tests.rs`
- `crates/core/src/server/mod.rs`
- `crates/core/src/server/file_tree_handler.rs`
- `crates/core/src/server/config.rs`
- `crates/fs/src/lib.rs`
- `crates/cli/src/commands/reverse.rs`

## Findings

`ReverseBroker` is already application-neutral. It authenticates outbound
streams, checks one exact peer principal, and pairs each accepted stream with a
loopback client connection without interpreting the carried 9P messages.

`FilesystemExport` owns two different concerns in one type:

- the generic outbound lifecycle: connect, authenticate, maintain a bounded
  pool, reconnect with bounded backoff, stop active streams, and serve one 9P
  session per stream; and
- the filesystem-specific factory that opens a fresh `fs::LocalTree` for each
  session.

The generic server entry point already exists as
`r9p::server::serve_file_tree_connection`. Its only backend requirement is a
caller-supplied `FileTree`; `ServerConfig` is cloneable and already carries the
bounded message, fid, variant, and asynchronous-request settings.

The narrow reusable extraction is therefore a reverse exporter that accepts a
validated transport configuration, a `ServerConfig`, and a thread-safe factory
for fresh application-owned `FileTree` instances. `FilesystemExport` can remain
the local-filesystem convenience adapter by validating its root and delegating
to that generic exporter.

No new wire protocol, broker posture, listener, namespace policy, or service
registration belongs in this extraction. The application still owns the
meaning and lifecycle of the tree it supplies.

## Effect

Add the generic tree-export seam to `r9p-reverse`, prove it with an in-memory
tree over the real authenticated reverse broker, and keep the CLI filesystem
command as a small specialization over the same path.

## Open Questions

Application trees with blocking reads may need the asynchronous
`ConnectionHandler` server seam rather than the synchronous `FileTree` adapter.
The first consumer should use bounded nonblocking reads, so that broader
extraction is not required yet.
