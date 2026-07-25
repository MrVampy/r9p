# FUSE Lookup Fid Pressure

Date: 2026-07-25

## Question

Why can a recursive traversal of a mounted dynamic namespace exhaust the
server's live fid capacity before Linux sends FUSE forget requests?

## Sources Inspected

- `crates/fuse/src/fuse/ops/lookup.rs`, especially `lookup_once` and
  `forget_node`.
- `crates/fuse/src/fuse/mount_state.rs`, especially `fresh_child_fid`,
  `fresh_node_fid`, and `bound_node_fid`.
- `crates/fuse/src/node.rs`, especially `NodeTable::insert_lookup`,
  `NodeTable::insert_lookup_lazy`, and `NodeTable::forget`.
- `crates/core/src/server/config.rs` and
  `crates/core/src/server/session.rs`, especially the bounded live-fid
  accounting.
- `refs/coordinator/refs/9pfuse/main.c`, especially `fuselookup`,
  `fuseforget`, and the nodeid fid ownership comments.

## Findings

The current Rust bridge kept one unopened 9P fid in every node created by a
FUSE lookup. Linux may cache more than the server's bounded 4096 live fids
before issuing forget requests, so a fast recursive walk can exhaust the
connection even though it eventually releases its dentries.

The plan9port `9pfuse` reference also retains a fid per returned nodeid until
forget and explicitly notes unfinished fid reference counting. That behavior
does not account for this workspace's deliberately bounded server session.

The Rust bridge already records canonical path lineage and already creates
path-only nodes for READDIRPLUS. Every actual open uses a fresh path walk.
Therefore lookup can retain the stat and path while clunking its transient walk
fid immediately. A later operation can bind a fresh fid from the recorded path.

## Effect

FUSE lookup now stores a path-only node and releases both the transient lookup
fid and any older cached node binding before replying. Open handles retain
their independent fids, so file-session semantics are unchanged. Forget still
owns Linux node reference accounting but no longer controls the lifetime of a
fid for ordinary lookup-only nodes.

## Open Questions

An integration stress test against a server configured below the Linux dentry
working set would provide additional end-to-end coverage beyond the node-table
regression and live mounted-namespace proof.
