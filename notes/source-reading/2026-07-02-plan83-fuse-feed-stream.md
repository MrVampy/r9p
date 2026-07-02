# Plan 83 FUSE Feed Stream Slice

Date: 2026-07-02.

Question: how should `r9p mount` consume the stream-primary namespace feed without turning the FUSE projection into a Vault registered service?

## Sources Checked

- Vault `docs/plan/83/index.md`: FUSE is a projection over the door-attached session, not a registered service; FUSE-specific nodeid, lookup count, handle, readdirplus, and kernel invalidation behavior stay at the FUSE edge.
- Vault `docs/plan/83/index.md`: the session layer should consume `/events/namespace/stream` as the primary feed and use recent/since polling as fallback.
- `crates/session/src/feed/worker.rs`: the session worker processes stream records directly, while poll fallback selects records against the remembered event id.
- `crates/fuse/src/fuse/change_feed.rs`: the existing FUSE change-feed consumer already applied generic namespace-change records into FUSE-local node staleness and kernel invalidation, but it only used poll-style reads.
- `crates/fuse/src/fuse/status.rs`: the mount status file reported connected/degraded state but did not identify whether the active feed source was stream or poll.
- `crates/cli/src/commands/mount.rs`: `r9p mount` owned its lifecycle/status options and parsed change-feed poll options, but lacked `--change-feed-stream`.
- `crates/fuse/src/fuse/ops/io/open.rs`: cached directory snapshots are served from the FUSE `NodeTable` before a fresh remote readdir when the cache is still fresh.
- `crates/fuse/src/node.rs`: parent entry lookup could generate kernel invalidation targets, but it did not clear the FUSE-owned parent directory cache for a created child that did not yet have a node.
- `crates/fuse/src/fuse/invalidation.rs`: kernel invalidation already drops parent readdir caches when a parent entry tuple is present; this does not clear `NodeTable`'s own cached directory snapshot.

## Findings

- The runtime must still see FUSE as an ordinary attached 9P client. No core namespace path or `/srv` registration is needed for this slice.
- Stream and poll cannot share cursor rules. A stream fid owns its cursor and delivered records are processed directly; a poll read must still select records against the last observed event id unless a since-template path is configured.
- FUSE should reuse the generic session feed record parser and cursor selector, but keep event application local because kernel invalidation, stale bindings, and node-table rebinds are FUSE projection state.
- Status needs a feed source field. Without it, a mount using stream-primary could silently fall back to poll and still report only `change_feed: "connected"`.
- Stream-driven invalidation must clear two layers for directory membership changes: the kernel parent entry or readdir cache and the FUSE-local parent directory cache. Kernel invalidation alone leaves `NodeTable::cached_dir_entries_if_fresh` able to serve a stale root snapshot until TTL expiry.

## Effect

- Add `change_feed_stream_path` to the FUSE mount config.
- Add `r9p mount --change-feed-stream PATH`, matching the existing `r9p session serve --change-feed-stream PATH` vocabulary.
- Refactor the FUSE change-feed loop so stream consumption is primary and poll is fallback.
- Add `source: "stream"` or `source: "poll"` to the FUSE status JSON.
- Keep FUSE node state, open handles, stale binding clunks, and kernel invalidation in `crates/fuse`.
- Add `NodeTable::mark_parent_directory_cache_stale` and use it when applying `created`, `removed`, `renamed`, and `modified` namespace change records. It returns the same parent entry tuple used for kernel invalidation while clearing the FUSE-owned parent directory cache.
- Add a focused host-gated FUSE integration test with a synthetic 9P namespace. The test warms a long-TTL root directory cache, writes to a 9P `/mutate` control file, serves the resulting namespace event over `/events/namespace/stream`, and requires `created.txt` to appear through the mount before TTL expiry.

## Proof

- `cargo check -p cli`
- `cargo test -p fuse`
- `cargo test -p cli`
- `cargo test -p cli --test cli_machine`
- `cargo fmt --all --check`
- `cargo test -p session`
- `cargo test -p cli --test session_control`
- `cargo test -p cli --test fuse_mount -- --ignored --test-threads=1`
- `git diff --check`
- Live M7 proof against `192.168.0.30:9564`: a temporary `r9p mount` was started with `--change-feed /events/namespace/recent`, `--change-feed-stream /events/namespace/stream`, and `--change-feed-cursor-template '/events/namespace/since/{event_id}'`.
- The FUSE status file reported `change_feed: "connected"`, `source: "stream"`, and last event id `5kvwlr7caxtx46gy5p7urvncgdp5dknl:0000000000000009`; a shallow root listing through the mount returned namespace entries including `boot`, `capabilities`, `catalog`, and `credentials`.
- `cargo test -p fuse node::tests::parent_directory_cache_clears_for_child_namespace_change`
- `cargo test -p cli --test fuse_stream_invalidation -- --ignored --test-threads=1`
- The new ignored stream-invalidation test proves a real 9P write-triggered namespace mutation invalidates a warmed long-TTL FUSE directory cache through the stream feed.

## Open Questions

- A later Plan 83 slice still needs to host FUSE inside the long-lived `r9p session` process when shared cache across session queries and FUSE is required.
- The mutation proof is currently synthetic but protocol-level: it uses a real 9P server, real `r9p write /mutate`, and a real FUSE mount with stream feed. A future M7 proof can repeat the same pattern when a disposable namespace mutation surface is available.
