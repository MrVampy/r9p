# Materialization startup invalidation coalescing

Date: 2026-08-14

## Question

How should a coherent materialization recover when several feed coherence-loss
events accumulate while a full snapshot is still being built?

## Sources inspected

- `crates/session/src/materialization.rs`: initial snapshot ordering,
  `drain_startup_events`, complete-batch cursor publication, and event-loop
  recovery.
- `crates/session/src/materialization/local.rs`: cursor invalidation, staged
  snapshot publication, durable state publication, and reusable-tree
  validation.
- `crates/session/src/feed/worker.rs`: connection-loss, malformed-record,
  cursor-loss, and backpressure invalidations.
- `crates/session/src/feed/event.rs`: bounded subscriber queues and retained
  events before disconnection.
- `crates/session/src/feed/state.rs` and `feed/record.rs`: feed cursor state and
  strict event ID admission.
- Memory's `crates/service/src/namespace/content.rs`,
  `namespace/tree.rs`, and `store/journal.rs`: bounded recent changes,
  cursor-addressed catch-up, and blocking change-stream behavior.

## Live evidence

- M7's Memory materialization contained 1,407 files and took about 34 seconds
  to build and publish one complete snapshot.
- The staging directory advanced from `.snapshot-2556002-27` directly to
  `.snapshot-2556002-28`; `.state.json` remained absent.
- Each snapshot completed successfully, updated the stable tree, and
  immediately started the next full snapshot.
- The direct Memory change stream remained connected and blocked normally,
  while `changes/recent` returned 60 valid records in 9,845 bytes.
- The retained startup queue therefore outlived the transient coherence loss.
  The materializer was replaying one full snapshot per already-queued coarse
  invalidation rather than recovering once from the bounded event batch.

## Findings

- `drain_startup_events` previously performed `synchronize` immediately for
  every `CoarseInvalidation`, resync record, rename record, or subscriber
  disconnection.
- A transient feed failure can enqueue several invalidations while one
  expensive snapshot is in progress. Serially rebuilding once per retained
  invalidation turns bounded recovery work into an arbitrarily long startup
  delay even after the feed is healthy.
- The event bus already bounds the available batch. Draining that batch before
  recovery permits all included coherence-loss signals to collapse into one
  full snapshot without weakening the fail-closed contract.
- A durable cursor remains admissible only when a complete change record occurs
  after the last coarse invalidation or resync record. Subscriber disconnection
  occurs after its retained queue is drained, so it invalidates every cursor in
  that batch.
- Events arriving during the coalesced snapshot remain queued for the next
  bounded drain. They are applied incrementally or trigger one further
  resnapshot, preserving stream-before-snapshot coverage.

## Effect

- Startup and event-loop recovery drain each currently available bounded event
  batch before choosing recovery work.
- Any number of coherence-loss signals in one batch cause exactly one complete
  resynchronization.
- Cursor state advances only from mechanically proven complete feed records
  after the final coherence loss in the batch.
- Ordinary change-only batches retain incremental application and durable
  cursor advancement.

## Open questions

- None for the current bounded complete-tree consumer.
