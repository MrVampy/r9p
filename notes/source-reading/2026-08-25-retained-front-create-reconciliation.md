# Retained Front create reconciliation

## Question

How should a create relay complete when its authoritative backend accepted a
create for a path that a standing Front retained across an owner restart?

## Sources inspected

- `refs/plan9port/src/lib9p/fid.c`: fid lifetime and removal ownership.
- `notes/source-reading/2026-08-22-open-fid-after-remove.md`: retained Front
  node and qid lifetime after namespace removal.
- `crates/front/src/model.rs`: `State::insert_created_relay_node`, detached
  node retirement, and qid indexing.
- `crates/front/src/front.rs`: `Front::complete_create_with`.
- `crates/front/src/tree.rs`: create relay admission and fid rebinding.
- Coordinator `src/core/api/r9p/front_feed_publisher.gleam`: standing-door
  recovery publication.

## Findings

A standing Front deliberately retains service paths while a restarted owner
reconstructs its registry. The client create still enters the authoritative
backend. When that backend accepts the same logical object and returns the same
qid, the Front must reconcile the retained node instead of treating its qid as
an unrelated collision. Rejecting it leaves the client in a create-retry loop
even though authority already accepted the operation.

This is narrower than general create idempotency. A retained node is reusable
only when the existing child has the exact returned qid and kind. A different
qid or kind remains an error. The reconciled file stops serving retained bytes,
becomes the accepted write relay, and receives fresh published bytes after the
owner processes the write. Existing fids keep the same node and qid, so they
either see an explicit non-file state during recreation or the fresh report;
they never see retained bytes as current.

## Effect on r9p

`State::insert_created_relay_node` now reconciles an exact retained child before
performing the ordinary new-child collision checks. A regression holds an open
fid across the accepted recreation, proves retained bytes stop being served,
and proves the same fid reads the newly published report afterward.

## Open questions

None for exact same-qid standing Front recovery. A backend that returns a new
qid for the same retained name is a distinct remove-and-recreate operation and
still needs an explicit owner-level replacement contract.
