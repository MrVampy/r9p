# Pipelined same-fid RPC

Date: 2026-07-24.

## Question

Can an interactive namespace client remove the idle network round trip between
writing a complete same-fid RPC request and reading its response without
inventing another transport?

## Sources inspected

- `crates/core/src/blocking.rs`
- `crates/core/src/multiplex/client.rs`
- `crates/core/src/server/connection.rs`
- `crates/session/src/client.rs`
- `crates/session/src/opened_fid.rs`
- `crates/auth/src/handshake.rs`
- `crates/reverse/src/lib.rs`
- Agents `crates/runner/src/service/transport.rs`
- Agents `crates/runner/src/service/tree/mod.rs`

## Findings

The bounded TCP client, authenticated Agents listener, and reverse-connect
transport already enable `TCP_NODELAY`. The remaining interactive delay was
therefore not an omitted socket option.

The multiplexed client previously waited for `Rwrite` before sending `Tread`.
For Agents same-fid RPC files, the r9p server connection handles ordinary
writes and reads synchronously in wire order. The write buffers the complete
request before the following read executes it and returns the typed response.
Sending both tagged requests before awaiting either reply preserves that
ordering while removing one otherwise idle round trip.

## Effect

The multiplexed client and retained-fid session facade now expose an opt-in
pipelined write/read operation with bounded delimiter framing. Prefix write
chunks still complete before the final pipelined pair. The API documents that
callers may use it only where the server contract processes the write before
the subsequent read on the same fid.

## Open questions

Live terminal measurements must determine how much end-to-end latency remains
after combining pipelined input RPCs with already-posted adjacent update reads.
