# Plan 83 Session Cache Directory Stat Seeding

Date: 2026-07-02.

Question: why did warm session snapshots over `/srv` still take about 1.4 seconds after the directory cache had been populated?

## Sources Checked

- Vault `docs/plan/83/index.md`: slice 5 and slice 7 require warm session snapshots to answer from the session view in milliseconds and report cache/freshness honestly.
- `crates/session/src/control/snapshot.rs`: recursive snapshots use `stat_for_path` for each visited path and `directory_entries_for_path` for directories, with cache hit/miss counters in the response.
- `crates/session/src/cache.rs`: `update_directory` cached the listing for the directory itself but did not seed child `Stat` records from the 9P directory entries.
- Live M7 door at `192.168.0.30:9564`: used to measure current raw 9P, standalone FUSE, and temporary `r9p session serve` behavior.

## Findings

- 9P directory reads already carry stat records for each returned child. Not caching those child stats made recursive session snapshots re-stat each child even when the parent directory listing was fresh.
- The cache counters exposed the problem directly: before the fix, warm `/srv` depth-2 snapshots still reported many stat and directory misses and took about 1.37 seconds.
- The correct cache owner is the shared `NamespaceCache`, not the snapshot renderer. Seeding child stats when a directory cache is updated benefits list and snapshot callers without teaching them new cache rules.

## Effect

- `NamespaceCache::update_directory` now seeds fresh child stat entries from returned directory entries.
- If an existing child qid changes, the child's directory cache is cleared so old child listings are not reused across identity changes.
- Added cache regression tests for child stat seeding and replaced-child directory cache clearing.

## Live Evidence

- Raw fresh attach `ls /`: about 0.077 seconds.
- Current standalone FUSE root depth-1 listing: about 0.713 seconds on the sampled run.
- Session `/srv` list before this slice showed the intended cache shape: about 0.053 seconds cold and about 0.005 seconds warm.
- Before this slice, session `/srv` depth-2 snapshot remained about 1.37 seconds warm.
- After this slice, session `/srv` depth-2 snapshot reported `stat_hits=37`, `stat_misses=0`, `dir_hits=14`, `dir_misses=0` and completed in about 0.009 seconds once warm.
- After this slice, root depth-1 snapshot repeated warm in about 0.005 seconds.

## Proof

- `cargo fmt --all --check`
- `cargo test -p session`

## Open Questions

- Cold recursive snapshot is still bounded by server-side namespace evaluation. Plan 83 slice 7 still needs the full concurrency and front-materialized versus brain-evaluated matrix before deciding whether a broader core primitive is warranted.
