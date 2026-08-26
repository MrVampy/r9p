# Beam Port Ordinary Request Concurrency

Date: 2026-08-26

## Question

Why can several independent BEAM callers time out when they share the ordinary
r9p native port, even though responses carry request identities and the 9P
client supports tagged concurrency?

## Sources Inspected

- `crates/beam-port/src/stdio.rs`: `run`, `ResponseWork`, and response writing.
- `crates/beam-port/src/lib.rs`: `PeerClientServer::dispatch_line`, cached
  namespace clients, and retry handling.
- `bindings/gleam/src/r9p_beam_port_ffi.erl`: `request_on`, `server_loop`, and
  tagged response dispatch.
- `crates/session/src/client/namespace.rs`: cloneable namespace client state.
- `crates/session/src/client/direct.rs`: cloneable multiplexed direct client.
- `crates/beam-port/tests/stdio_multiplex.rs`: existing Front concurrency proof.

## Findings

The Erlang port server already admits several caller requests and correlates
responses by request identity. The Rust stdio adapter then defeated that
concurrency for ordinary client operations: its stdin loop called
`PeerClientServer::dispatch_line` synchronously, and network work completed
before the loop read the next request. Only a pending Front request was moved
to another thread.

The namespace client is cloneable. Its direct sessions use the core multiplexed
client, and its namespace state protects routes and fids internally. Independent
ordinary operations can therefore share one cached namespace session without
serializing the stdio intake loop.

A shared client cache must not hold its map lock while connecting or performing
9P work. Concurrent initial connections may race, but the cache can retain one
session and drop the unused connection. On failure, removal must compare session
identity so a late failure cannot evict a newer replacement.

## Effect

The Beam port uses a bounded ordinary worker queue. Front ownership remains on
the intake thread, ordinary network operations execute concurrently through
cloned multiplexed namespace clients, and tagged stdout responses may complete
out of order. Queue capacity bounds memory and applies backpressure to port
stdin instead of spawning an unbounded thread per call.

An end-to-end regression holds one same-fid RPC response open and requires an
independent stat over the same cached namespace session to complete first.

## Open Questions

- Production measurements should determine whether the fixed worker and queue
  bounds need configuration rather than constants.
- Caller deadlines still need to reflect service contracts. Concurrency removes
  head-of-line blocking but does not make an intrinsically slow mutation fast.
