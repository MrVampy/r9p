# FUSE subtree change-feed paths

Date: 2026-08-28

## Question

How should a FUSE mount rooted at one namespace subtree apply absolute paths
from the shared namespace change feed?

## Sources inspected

- `crates/fuse/src/fuse/change_feed.rs`
- `crates/fuse/src/fuse/mount_state.rs`
- `crates/fuse/src/node.rs`
- `crates/session/src/feed/`
- `crates/fuse/src/fuse/tests.rs`

## Findings

The remote walk correctly prepends `Config.source_path`, while `NodeTable`
stores every local node path relative to the mounted root. The change-feed
consumer parsed each absolute event path and passed it to that relative node
table unchanged. Precise invalidation therefore worked for a root mount but
missed descendants of a non-root mount.

A change below the selected source must strip the source prefix before node
lookup. A change at or above the selected source invalidates the entire mount.
A sibling change is outside the mounted namespace and has no local cache
effect. Rename handling must also distinguish moves within, into, and out of
the mounted subtree.

## Effect

The FUSE adapter now projects feed paths into mount-relative paths before
kernel invalidation. This remains a generic FUSE concern; services continue to
publish one absolute namespace change feed and do not learn which subtrees
clients mount.

## Open questions

None for the current contract.
