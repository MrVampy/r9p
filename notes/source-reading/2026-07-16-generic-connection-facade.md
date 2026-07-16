# Generic Connection Facade

Date: 2026-07-16.

## Question

Which parts of a cancellable concurrent 9P connection loop are reusable r9p
machinery, and which responsibilities must remain with an application runtime?

## Sources Inspected

- `crates/core/src/codec.rs`: checked frame decoding and negotiated `msize`.
- `crates/core/src/server/types.rs`: split request admission and completion
  types.
- `crates/core/src/flush.rs`: generation-safe tag cancellation and stale
  completion rejection.
- `crates/front/src/serve.rs`: existing application connection loop,
  cancellation map, and asynchronous read workers.
- `refs/plan9port/man/man9/flush.9p`: `Tflush` ordering, immediate `Rflush`,
  and old-tag reuse rules.
- `refs/plan9port/src/lib9p/srv.c`: pending-request and flush-response
  coordination.

## Findings

Frame-size enforcement, per-connection session state, tag generations, flush
suppression of stale replies, fid-associated cancellation, version reset, and
bounded in-flight work are generic 9P connection concerns. Backend path
meaning, the choice of which requests may block, and the mechanism used to wake
a cancelled operation remain application decisions.

The cancellation map cannot double as a worker-capacity counter. `Tflush`
removes the pending tag before a cooperative handler necessarily exits. If that
removal releases capacity, a read/flush storm can create unbounded live workers
despite a nominal limit. Capacity therefore needs an independent permit held
until actual worker exit.

The connection facade must not own listener creation, socket paths or modes,
peer credentials, TLS, service admission, or daemon lifecycle. Those are
runtime policy, not 9P protocol state.

## Effect

`crates/core` exposes an optional `server::serve_connection` facade over any
cloneable `Read + Write` stream and a backend-neutral `ConnectionHandler`. It
uses negotiated frame bounds, generation-safe cancellation, and a finite
`max_async_requests` worker cap. Applications that already own an executor may
continue using `Server::admit` and `Server::complete` directly.

Regression coverage holds one cancelled-but-not-exited handler open while a
read/stat/flush storm attempts to recycle capacity, proving the active-worker
count never exceeds the configured cap.

## Open Questions

The current facade deliberately uses bounded OS threads. A future executor
adapter can implement the same admission/completion contract without changing
the protocol state machine.
