# Plan 83 Feed Extraction Slice

Date: 2026-07-02.

Question: which parts of the existing FUSE namespace change-feed consumer are generic session-manager machinery, and which parts must remain FUSE projection behavior?

## Sources Checked

- Vault `docs/plan/83/index.md`: slice 6 says the session manager should consume the already-live `/events/namespace` feed, with blocking stream primary and `since/<id>` fallback later.
- `crates/fuse/src/fuse/change_feed.rs`: current FUSE-only feed consumer parses namespace-change records, handles cursor fallback, opens feed paths, applies kernel invalidations, records mount diagnostics, and manages reconnect.
- `crates/fuse/src/fuse/invalidation.rs`: kernel invalidation is FUSE-specific and must not move into the session crate.
- `crates/fuse/src/node.rs`: stale path marking and FUSE node state are projection-owned.
- `crates/session/src/lib.rs`: existing shared session crate owns reusable client/cache/request primitives and is the right owner for feed record parsing that other access projections will need.

## Findings

- Record parsing, absolute namespace path parsing, scope matching, poll-path construction, and cursor-window selection are generic session machinery. FUSE was the first consumer, but these functions are not tied to Linux FUSE.
- Applying a feed event remains FUSE-owned: it marks FUSE node bindings stale, sends kernel invalidations, clunks stale fids, writes mount diagnostics, and coordinates data-client reconnects.
- Cursor-miss behavior has a generic and a projection half. The generic half is detecting that the recent window no longer contains the previous cursor and advancing to the latest visible event id. The FUSE half is responding with coarse invalidation.
- Extracting the generic half reduces `crates/fuse/src/fuse/change_feed.rs` from 559 lines to the low 300s and gives the session crate direct tests for feed parsing and cursor selection.

## Effect

- Add `crates/session/src/feed.rs` with `NamespaceChange`, `SelectedFeedRecords`, record parsing, path parsing, scope matching, poll-path construction, and cursor selection.
- Keep FUSE event application and degradation handling in `crates/fuse/src/fuse/change_feed.rs`.
- Move generic feed parser tests from FUSE into the session crate; keep the FUSE test that checks feed timeout errors do not force data-client reconnect.

## Open Questions

- The next Plan 83 slice should add a session-level feed owner or status state that tracks feed state, last event id, and degraded reasons without depending on FUSE.
- Blocking stream consumption is still not implemented here; this slice only moves the generic parser/cursor machinery needed by both FUSE and the future session owner.
