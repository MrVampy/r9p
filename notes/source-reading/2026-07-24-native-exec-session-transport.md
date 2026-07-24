# Native Exec Session Transport

Date: 2026-07-24.

## Question

What is the smallest generic r9p shape needed to carry a native, bidirectional
stdio protocol from a remote operating host without turning 9P into a file
transfer protocol or adding application-specific behavior to r9p?

## Sources Inspected

- `crates/core/src/server/connection.rs`, especially `ConnectionHandler`,
  asynchronous dispatch, cancellation, and connection reset
- `crates/core/src/multiplex/client.rs`, especially concurrent tagged calls,
  retained fids, and whole-connection shutdown
- `crates/session/src/client.rs`
- `crates/cli/src/commands/con.rs`
- `crates/reverse/src/broker.rs`
- `crates/reverse/src/export.rs`
- `notes/source-reading/2026-07-22-reverse-connect-9p-transport.md`
- `notes/source-reading/2026-07-23-generic-reverse-tree-export.md`
- `notes/source-reading/2026-07-23-retained-fids-for-interactive-channels.md`

## Findings

- A bidirectional application stream is one logical 9P session. Opening stdin
  and stdout through separate client connections can accidentally create two
  application sessions even when both paths name the same file.
- The multiplexed client already permits concurrent reads and writes on
  distinct retained fids over one transport. `r9p con` can use that directly
  instead of creating a second blocking client for its writer thread.
- The reverse broker is a byte-pairing runtime adapter. Supporting a Unix
  client endpoint does not change its network authentication, reverse pool, or
  9P semantics, and it gives sandboxed local consumers an exact socket that can
  be bound into one process unit.
- A blocking stream read must use the asynchronous `ConnectionHandler` server
  seam so writes, flushes, and cancellation remain live while one read waits.
  A `FileTree` mutex is the wrong generic presentation for this case.
- The reverse exporter can accept a fresh application-owned
  `ConnectionHandler` per authenticated stream while continuing to own only
  connection, authentication, pool, retry, and shutdown lifecycle.
- Process selection, argv, environment, sandboxing, and stream meaning remain
  application policy. r9p should expose the generic connection mechanics but
  must not learn about agents, terminals, or exec servers.

## Effect

- `ReverseBroker` now supports either a loopback TCP proxy or an absolute Unix
  proxy socket.
- `ReverseExport::start_handler` serves a fresh application-owned
  `ConnectionHandler` on every authenticated reverse stream.
- The session client exposes the existing no-deadline retained-fid operations.
- `r9p con` uses one multiplexed connection for its read and write fids.
- M7 tests cover the Unix proxy, application-owned handler, shared session
  client, CLI, and existing reverse filesystem behavior.

## Open Questions

The first application consumer must still prove bounded buffering, cancellation,
child-process teardown, and fail-closed capability withdrawal. Those are
consumer responsibilities rather than additions to the r9p contract.
