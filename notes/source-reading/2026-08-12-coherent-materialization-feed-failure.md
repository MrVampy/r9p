# Coherent materialization feed failure

Date: 2026-08-12

## Question

How must a namespace change feed behave when it protects a persistent local
materialization rather than only a short-lived FUSE cache?

## Sources inspected

- `crates/session/src/feed/worker.rs`: stream, catch-up, reconnect, parsing,
  backpressure, and event publication.
- `crates/session/src/feed/event.rs`: bounded subscriber queues and coarse
  invalidation delivery.
- `crates/fuse/src/fuse/change_feed.rs`: consumer-side scope filtering and
  coarse kernel-cache invalidation.
- `crates/session/src/client_session.rs`: renewable attachment and explicit
  consumer-owned reconstruction after reconnect.
- `crates/fuse/src/fuse/ops/io/open.rs`: one remote open per kernel open.
- `crates/fuse/src/fuse/util.rs`: direct I/O versus coherent kernel page-cache
  selection.
- `crates/session/src/materialization.rs` and
  `crates/session/src/materialization/local.rs`: the extracted local mirror,
  snapshot bounds, update application, and filesystem confinement.

## Findings

- `process_feed_data` previously discarded every unparseable non-empty record
  through `filter_map`. A coherent consumer could then retain stale bytes while
  the server-side stream cursor had already advanced.
- A stream transport failure changed only diagnostic state. Consumers received
  no event that made an already materialized tree unavailable while catch-up
  and reconnect were pending.
- The existing `CoarseInvalidation` event is the correct generic recovery
  signal. It does not invent service semantics: a consumer reconstructs its
  own derived state from the authoritative namespace.
- Per-run FUSE page caches cannot preserve warm metadata or content between
  bounded Agent runs. A persistent local materialization must therefore treat
  malformed records, subscriber overflow, and stream loss as loss of coherence
  and perform a complete resynchronization.
- Full eager materialization and FUSE are different cache backends. The former
  is appropriate for a declared bounded read tree that must be immediately
  available to native tools; FUSE must remain lazy for large trees and retain
  its separate writable mode. Both should consume one feed parser, reconnect
  state machine, cursor contract, and coarse-invalidation event.
- The session worker previously opened catch-up before the replacement stream.
  Opening the stream first is required so mutations arriving during catch-up
  are retained by the stream fid rather than falling into a reconnect gap.

## Effect

- The session feed now emits coarse invalidation on malformed bytes, malformed
  records, and stream connection loss.
- Malformed input is not interpreted as a path mutation. Consumers fail closed
  to full reconstruction.
- r9p now owns the bounded local materialization as reusable session machinery.
  Agent supplies lifecycle, logical paths, limits, and read-only bind policy;
  it no longer implements snapshot or cache behavior.
- FUSE's direct feed path now consumes the same session feed worker and event
  bus as the materializer. Kernel caching stays lazy while feed parsing,
  stream-before-catch-up ordering, reconnect, strict decoding, backpressure,
  and coarse invalidation have one implementation.

## Open questions

- A future large-tree consumer may need a bounded on-disk block cache rather
  than either an eager complete materialization or the kernel's lazy FUSE
  cache. That is a separate storage policy over the same feed contract.
