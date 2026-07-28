# Terminal Multiplex Transport State

Date: 2026-07-28

## Question

Why can a blocking namespace observation take its entire cleanup timeout after
the remote 9P listener has already closed the TCP connection?

## Sources Inspected

- `crates/core/src/multiplex/client.rs`
  - `MultiplexedClient::submit_message`
  - `MultiplexedClient::shutdown`
  - `MultiplexedInner::drop`
- `crates/core/src/multiplex/reader.rs`
  - `reader_loop`
  - `read_response`
- `crates/core/src/multiplex/util.rs`
  - `fail_all`
- `crates/session/src/opened_fid.rs`
  - `OpenedFid::drop`
  - `OpenedFid::clunk`
- `crates/session/src/error.rs`
  - `is_transport_message`
  - `transport_errno`
- `refs/plan9port/src/libmux/mux.c`
  - `muxrpc`
- `refs/plan9port/src/libmux/io.c`
  - `_muxrecv`
- `refs/plan9port/src/libmux/queue.c`
  - `_muxqhangup`
- `refs/plan9port/src/lib9pclient/close.c`
  - `fidclunk`

## Findings

The r9p reader thread detected peer EOF and failed every request that was
pending at that instant. The shared multiplexed client did not retain that
terminal transport error, however. A later cleanup `Tclunk` could therefore
register a new waiter after the reader thread had exited. No thread remained
that could complete or fail the new waiter, so the cleanup call lasted until
its response deadline.

The waiter map and the terminal reader state must share one lock. Otherwise a
submission can race between checking terminal state and registering its
waiter. Once the reader terminates, the state atomically records the first
terminal error and drains all current waiters. Every later submission receives
the same error without writing another request.

The plan9port multiplexer treats EOF as terminal for the active RPC. Later
operations encounter the same dead file descriptor rather than becoming
unobservable waiters. r9p needs the equivalent property even though it uses a
dedicated reader thread and per-tag channels.

The session error adapter also omitted the existing
`9P transport closed before response` error from its transport classification,
so it reported a remote-I/O errno rather than `ENOTCONN`.

## Effect

The core multiplexer now owns terminal response state beside its waiter map.
Peer EOF, explicit shutdown, and write failure terminate that state, wake all
current callers, and reject later calls immediately. The session adapter maps
the terminal transport error to `ENOTCONN`.

## Open Questions

None for this defect. Live Coordinator restart recovery remains the
end-to-end proof that bounded cleanup no longer hides prompt peer closure.
