# Detached Front qid republication

Date: 2026-08-26

## Question

How should a retained Front handle owner republication of a pushed path whose
node remains detached because an existing fid still references it?

## Sources inspected

- `crates/front/src/model.rs`, especially `State::place_pushed_file`,
  `State::place_pushed_directory`, `State::retire_detached_subtree`, and qid
  indexing.
- `crates/front/src/front.rs`, especially pushed publication and subtree
  retirement.
- `notes/source-reading/2026-08-10-atomic-front-recovery.md`.
- Coordinator `src/core/api/r9p/front_feed_publisher.gleam`, especially
  write-before-prune recovery publication.
- Coordinator `src/core/api/r9p/rust_port_listener.gleam`, especially standing
  listener adoption across a runtime-brain restart.
- Coordinator `docs/operations/9p-endpoint.md`, especially retained Front and
  held-fid continuity across runtime handoff.

## Findings

- Subtree retirement deliberately retains a detached node and its qid while a
  fid references that node.
- Pushed republication previously treated any retained qid as a collision when
  the logical path was absent from the active tree. A publisher could therefore
  not restore the same logical node until every old fid clunked.
- The live M7 Coordinator handoff hit this state as `qid path already in use`.
  Front publication failed, the create relay stayed unavailable, and Agent
  registration could not complete.
- A publisher-owned qid is explicit identity. If the retained qid belongs to a
  detached node at the same canonical path and with the same file or directory
  kind, republication is continuation of that node, not a collision.
- A retained qid owned by another path or an incompatible node kind remains a
  hard collision. A true remove-and-recreate operation must use a new qid.

## Effect on implementation

- Pushed file and directory publication reattach the exact detached node when
  qid, canonical path, and node kind agree.
- Reattachment removes the stale parent link, reconnects the node to the active
  parent, preserves held fid identity, applies current metadata, and collects
  any detached ancestor that no longer has fid references.
- Regressions cover same-path continuation and rejection of cross-path qid
  reuse.

## Open questions

- Coordinator service registrations should continue to use their registry
  generation when a true service incarnation needs a new qid rather than
  relying on path identity alone.
