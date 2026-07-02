# Plan 83 Session Cache Slice

Date: 2026-07-02.

Question: how should the door-attached session manager cache namespace structure without importing FUSE nodeid semantics or hiding denied branches?

## Sources Checked

- Vault `docs/plan/83/index.md`: session cache holds in-memory structure only: names, qids, stats, directory snapshots, freshness, and no file-body caching or disk persistence.
- Vault `docs/plan/83/index.md`: FUSE keeps nodeid, lookup counts, per-open directory snapshot semantics, and kernel invalidation behavior at the FUSE edge.
- `crates/fuse/src/node.rs`: current FUSE node table combines path lineage, qid/stat freshness, directory cache, fids, node ids, generations, lookup counts, and rebind state.
- `crates/fuse/src/fuse/ops/io/open.rs`: FUSE uses cached directory entries only for directory-open behavior and pins per-open snapshots at the FUSE projection.
- `crates/session/src/cache.rs`: existing shared cache primitives covered freshness, directory entries, stat predicates, and directory decoding, but not a shared namespace cache owner.
- `crates/session/src/control/snapshot.rs`: snapshot/list/stat still live-walked every path even when a long-lived session process had already seen the directory.

## Findings

- The session-owned cache must store only validated path facts. A parent directory entry is useful for listing that parent, but it is not proof that walking or statting the child path is authorized.
- The first cache attempt used parent directory entries as child stat cache entries. The integration test caught the bug: `/denied` was listed as a normal entry instead of reported as a degraded denied branch. The correct rule is that child stat cache entries are created only after a successful child walk/stat.
- Directory cache invalidation belongs in the session feed owner. A namespace event for `/a/b` invalidates the changed path and descendants, and clears the parent directory cache for `/a`.
- The cache is useful only while feed coverage is connected. The control surface enables cache reads when feed state is `connected`; degraded or disabled feed states fall back to live reads.

## Effect

- Add `NamespaceCache` to `crates/session/src/cache.rs`.
- Share one `NamespaceCache` across local session control connections.
- Let the feed worker invalidate cache paths from namespace change records and mark the whole cache stale on cursor miss or backpressure.
- Make stat/list/snapshot use the cache when feed coverage is connected.
- Add cache counters to `status`, `stat`, `list`, and `snapshot` responses.
- Extend the CLI integration test so a second snapshot proves a warm root directory hit while still reporting the denied branch as degraded.

## Live Proof

- Local proof gate: `cargo fmt --all --check`, `cargo test -p session`, `cargo test -p cli --test session_control`, `cargo test -p fuse`, `cargo test -p cli --test cli_machine`, and `cargo check -p cli`.
- M7 proof against `192.168.0.30:9564`: a temporary local session owner was started with stream-primary feed options, waited until `feed.source: "stream"`, and then read two consecutive root snapshots at depth 1.
- First snapshot: `cache.enabled: true`, `stat_hits: 0`, `stat_misses: 38`, `dir_hits: 0`, `dir_misses: 1`.
- Second snapshot: `cache.enabled: true`, `stat_hits: 38`, `stat_misses: 0`, `dir_hits: 1`, `dir_misses: 0`.
- Status after both snapshots reported `cache.entries: 38`, `cache.directories: 1`, and `cache.stale_entries: 0`.

## Open Questions

- FUSE still keeps its own projection-local cache. Moving FUSE into the shared session process remains a later Plan 83 slice.
- The v1 cache has no `must_revalidate`, `max_age`, or `sync` request modes yet. Those belong in the query-options slice rather than in this structural cache owner.
