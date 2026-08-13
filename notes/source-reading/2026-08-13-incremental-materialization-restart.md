# Incremental materialization restart

Date: 2026-08-13

## Question

How can a long-lived consumer restart without downloading an unchanged
admitted namespace tree again, while keeping the local tree derived and
failing closed when change-feed lineage cannot be proven?

## Sources inspected

- `crates/session/src/materialization.rs`: stream-before-snapshot startup,
  event application, coarse resynchronization, and bounded remote discovery.
- `crates/session/src/materialization/local.rs`: stable live-tree inode,
  staged snapshot publication, incremental replacement, and filesystem
  confinement.
- `crates/session/src/feed/worker.rs`: stream-first reconnect, cursor catch-up,
  strict record parsing, event publication, and subscriber backpressure.
- `crates/session/src/feed/event.rs` and `feed/state.rs`: consumer queue and
  retained cursor state.
- `crates/fuse/src/fuse/change_feed.rs`: the lazy FUSE consumer of the same
  generic feed events.
- Memory's `crates/service/src/namespace/content.rs` and `namespace/tree.rs`:
  bounded complete change batches, `g<generation>-s<sequence>` cursor path,
  and explicit `resync` records when a cursor cannot be continued.

## Findings

- Agent's durable cache root was reused as a directory, but
  `CoherentMaterialization::connect` still took and downloaded a complete
  parallel snapshot on every supervisor start.
- The existing feed already has the right recovery contract: open the blocking
  stream before cursor catch-up, then consume the cursor-addressed one-shot
  batch. No polling or service-specific protocol is needed.
- One feed event can emit several path records with the same event ID. A
  cursor stored after the first path would let a crash skip the remaining
  paths permanently. Cursor completion therefore belongs to the complete
  parsed read batch, not an individual `FeedEvent::Change`.
- A persistent cursor is safe only when every preceding local mutation is
  durable. File replacement must sync the new file and containing directory
  before the cursor state is atomically renamed and the state directory is
  synced. Removal has the same parent-directory requirement.
- A full resynchronization must invalidate the old cursor before it begins to
  mutate the stable live-tree inode. Otherwise a crash could pair a partially
  published snapshot with the old apparently valid cursor.
- The cache identity is the exact logical materialization configuration and
  admitted namespace paths, not a physical service address. Coordinator may
  move the referred service without changing the logical authority.
- FUSE remains a lazy consumer. It needs the batch-completion field only as
  opaque feed metadata and continues translating changes into kernel cache
  invalidations.

## Effect

- The feed worker can seed its one-shot catch-up from a retained cursor and
  reports readiness only after the replacement stream is open and catch-up is
  complete.
- The materializer stores a strict, versionless internal state document beside
  the derived tree. It reuses the tree only when the configuration and bounded
  filesystem validate exactly.
- Complete feed batches advance the durable cursor after their local changes
  are durable. Invalid state, unsafe cursor text, cursor loss, malformed feed
  input, backpressure, or transport ambiguity triggers the existing full
  snapshot path.
- A first materialization still takes one full snapshot. Subsequent clean or
  crashed process starts normally perform only cursor catch-up and changed-file
  reads.
- The exact Tuxedo fleet closure exposed that materializer readiness had leaked
  into the public `FeedWorkerConfig`, breaking r9wm's independent feed client.
  Readiness and initial catch-up policy now travel through a crate-private
  startup object; generic feed consumers retain the consumer-owned public
  configuration contract.

## Open questions

- None for the current bounded complete-tree consumer. A much larger tree may
  still need the separate bounded on-disk block-cache policy already recorded
  in the coherent-materialization note.
