# Plan 83 Cache And Freshness Boundary

Date: 2026-07-02.

Question: which namespace cache pieces can move below the FUSE projection in Plan 83 slice 2 without turning the shared session layer into Linux FUSE code?

## Sources Checked

- Vault `docs/plan/83/index.md`: slice 2 requires shared qid/path/stat/directory freshness, while keeping Linux nodeid/generation details at the FUSE edge where possible.
- Vault `docs/architecture/61-door-attached-namespace-sessions.md`: the session manager is door-attached, owns local cache/freshness, and FUSE remains one projection.
- `docs/source-map.md`: current r9p source ownership and FUSE reference list.
- `crates/fuse/src/node.rs`: current mixed FUSE node table, path/qid/stat cache, directory cache, stale marking, and tests.
- `crates/fuse/src/node/handles.rs`: FUSE open-handle ownership.
- `crates/fuse/src/fuse/mod.rs`: cached stat and directory freshness checks, rebind logic, and invalidation path.
- `crates/fuse/src/fuse/ops/dir.rs`: `READDIR` and `READDIRPLUS` projection behavior.
- `refs/vault/refs/linux-fuse/include/uapi/linux/fuse.h` and `refs/vault/refs/libfuse/include/fuse_lowlevel.h`: node ids, generations, lookup counts, and readdirplus are Linux FUSE protocol responsibilities.

## Findings

- `DirEntry`, directory stat decoding, directory reads from an already opened 9P fid, and basic 9P stat predicates are generic session facts. They do not depend on Linux FUSE node ids or kernel lookup accounting.
- `qid_to_inode`, node ids, generations, lookup counts, open handles, `READDIRPLUS` reply encoding, and kernel invalidation remain FUSE-edge facts. Linux FUSE references explicitly define those fields and lifetime rules.
- Current `NodeTable` already separates some state by shape: `path`, `qid`, `stat`, `dir_cache`, and stale freshness are generic; `nodeid`, `generation`, `lookups`, and handles are FUSE projection state.
- Full session-manager status fields such as attached endpoint, last reconnect, and last change cursor need a long-lived session owner. Slice 2 should introduce reusable freshness records and move cache primitives first, not invent daemon state before the control surface exists.
- `crates/fuse/src/node.rs` is too large for continued work. Moving generic cache helpers into `crates/session` and splitting tests into `crates/fuse/src/node/tests.rs` is part of the slice, not optional cleanup.

## Effect

- Add a `session::cache` boundary for generic 9P directory entries, directory cache freshness, stat predicates, and directory read/decode helpers.
- Keep FUSE node-table ownership in `crates/fuse`, but make it store `session` cache types.
- Keep FUSE tests, nodeid/generation assertions, and handle tests at the FUSE edge.
