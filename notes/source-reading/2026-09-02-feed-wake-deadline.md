# Feed wake deadlines for endpoint retry

Date: 2026-09-02

## Question

How should a consumer of the shared namespace feed retry a known failed
endpoint when the endpoint returns after the last namespace change wake?

## Sources Inspected

- `crates/session/src/feed/event.rs`
- `crates/session/src/feed/worker.rs`
- `crates/session/src/materialization.rs`
- `docs/guides/event-driven-9p.md`

## Findings

`FeedWake` is the shared readiness primitive for namespace change feeds. A
consumer recovering a known failed endpoint sometimes needs to retry after its
bounded reconnect delay even when the namespace has not changed again. This is
distinct from polling namespace state: the consumer remains event-driven for
route changes and uses the deadline only to retry the already selected
endpoint.

## Effect

`wait_after_timeout` preserves the generation comparison and close semantics of
`wait_after`. It returns `Some(generation)` for a feed wake and `None` only when
the supplied deadline expires without a new generation.

## Open Questions

None.
