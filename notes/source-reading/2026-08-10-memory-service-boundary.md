# Memory Service Boundary Audit

Date: 2026-08-10

## Question

Should Memory's authenticated listener, Coordinator registration, reconnect,
and RPC retry behavior move into r9p as generic service-host machinery?

## Sources Inspected

- `docs/design/architecture.md`
- `docs/source-map.md`
- `docs/guides/event-driven-9p.md`
- `crates/core/src/server/`
- `crates/session/src/client_session.rs`
- `crates/session/src/client/paths.rs`
- `crates/session/src/opened_fid.rs`
- `crates/session/src/error.rs`
- `notes/source-reading/2026-07-28-pipelined-rpc-delivery-classification.md`
- Memory's `transport.rs`, `namespace_client.rs`, and `registration.rs`
- The analogous Agent, Terminal, and MCP service adapters

## Findings

- r9p owns framed 9P behavior, authentication for an already-created stream,
  session reconnect, cancellation, desired-state file reconciliation, and the
  generic distinction between a rejected RPC write and an unknown delivery.
- A service still owns its socket lifecycle, advertised endpoint, listener
  supervision, Coordinator registration document, renewal policy, and the
  meaning of registration failure. Moving those into r9p would make mechanism
  depend on application and Coordinator policy.
- Automatic RPC replay cannot be a generic transport decision. The consumer
  must identify whether its operation has an idempotent identity or a separate
  result-reconciliation path.
- Memory's Agent run submission is replay-safe because the run ID is stable and
  Agent returns the existing run. GitHub credential issuance is not replayed
  after an ambiguous response; Memory reports the outcome as unknown.
- The existing `WriteThenReadError` classification is the correct primitive.
  Memory should consume it directly instead of inferring delivery state from a
  generic transport error.

## Effect

No r9p code changed. Memory retained its application-owned listener and
registration adapters, removed generic mutation retry, and now selects replay
policy explicitly while preserving r9p's `Rejected` and `DeliveryUnknown`
outcomes.

## Open Question

If multiple services later converge on an identical listener adapter with no
endpoint, registration, retry, or supervision policy, that adapter may justify
a separate layered runtime utility. The current evidence does not support
putting it in r9p core.
