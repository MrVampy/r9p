# Namespace feed wake and reconnect

Date: 2026-07-28.

## Question

How can a namespace consumer wait for a registered service or Coordinator
connection to return without polling, while leaving operation replay decisions
with the consumer?

## Sources inspected

- `crates/session/src/client_session.rs`
- `crates/session/src/client/namespace.rs`
- `crates/session/src/feed/event.rs`
- `crates/session/src/feed/worker.rs`
- `crates/reverse/src/export.rs`
- r9wm `crates/terminal/client/src/client.rs`
- Coordinator `src/core/api/namespace/sources/journal/namespace_changes.gleam`
- Coordinator `/journal/changes/stream`

## Findings

`ClientSession` can renew a failed root attachment, and the namespace client can
renew a failed direct referral session. Neither mechanism knows when a
temporarily withdrawn namespace route has been published again. A consumer
that retries a missing path immediately therefore turns a temporary
registration gap into a busy loop.

The Coordinator already publishes a blocking namespace-change stream. The r9p
feed worker already consumes such a stream, catches up by event ID after a
stream interruption, and can fan changes out to consumers. A separate
application-specific reconnect feed would duplicate this mechanism.

The reusable consumer primitive is a wake generation driven by that stream. A
consumer records the generation before an operation, attempts the operation,
and blocks only if recovery requires a later namespace or feed-connection
event. All waiters observe the same generation without consuming an event from
one another.

The wake does not retry an operation. An exact read-only cursor may retry after
the wake, while a write whose delivery is unknown must reconcile its durable
operation ID. That distinction remains above r9p.

## Effect

The feed worker can publish a shared `FeedWake`. It advances the wake when the
blocking stream is established and when a namespace-change or coarse
invalidation event arrives. Closing the worker releases blocked waiters with a
shutdown result.

When a blocking stream fails, the worker renews its `ClientSession`, catches up
from the last event ID, and reopens the stream. Poll mode remains available for
callers that explicitly select it, but a configured stream no longer falls
through to a periodic poll after every interruption.

## Open questions

The application consumer still chooses which namespace changes are relevant.
If broad wake traffic becomes material, a later protocol-neutral filter can
match exact paths or prefixes without importing Coordinator service semantics
into r9p.
