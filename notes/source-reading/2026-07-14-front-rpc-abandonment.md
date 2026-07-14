# Front RPC Abandonment

Date: 2026-07-14.

## Question

Why did the Tradier sensor continue receiving thousands of RPC intake requests
after the original 9P clients had timed out, with every late completion rejected
as missing?

## Sources Inspected

- `crates/core/src/flush.rs`: `RequestTable::flush` and stale completion
  rejection.
- `crates/front/src/serve.rs`: read cancellation on `Tflush`, fid clunk, and
  connection close.
- `crates/front/src/tree.rs`: RPC request creation, same-fid replacement,
  clunk, remove, and `FrontTree::drop`.
- `crates/front/src/front.rs`: pending intake selection, RPC response waiting,
  timeout, and completion.
- `crates/front/bindings/deno/front_sink.ts`: Deno request intake and completion
  behavior.
- `crates/front/src/tests.rs`: existing RPC close, replacement, and round-trip
  coverage.

## Findings

The protocol core correctly rejects a stale completion after `Tflush`. The
front adapter removed the abandoned request's response slot, but several
abandonment paths left an unclaimed `IntakeRequest` in `State.pending`. A Deno
application could therefore receive canceled work, perform the provider call,
and only discover abandonment when `complete_request` returned `ENOENT`.

The affected paths were RPC clunk, connection-tree drop, same-fid request
replacement, relay removal cleanup, and RPC wait timeout or flush. This was a
generic front lifecycle defect, not Tradier request parsing or provider data.

## Decision

Every path that abandons an RPC response slot also removes an unclaimed pending
request with the same ID. A request already claimed by an application may still
finish late and receive `ENOENT`; that single stale completion is correct. The
fix prevents canceled requests that have not started from becoming an
application backlog.

Regressions cover clunk, tree drop, timeout, flush, and same-fid replacement
before application intake. No protocol compatibility path or
application-specific cancellation API is added.

## Open Questions

None for the unclaimed-request leak. Applications still need bounded provider
timeouts for work they have already claimed; that is a separate effect-boundary
concern.
