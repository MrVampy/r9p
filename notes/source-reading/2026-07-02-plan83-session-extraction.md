# Plan 83 Session Extraction Boundary

Date: 2026-07-02

Question: where should Plan 83's door-attached namespace session mechanics start, and what can move out of `crates/fuse` without changing mount behavior?

Files and functions inspected:

- Vault `docs/plan/83/index.md`: Claude and Codex amendments for `r9p session`, `crates/session`, mutation truth, daemon scope, and proof gates
- Vault `docs/architecture/61-door-attached-namespace-sessions.md`: door-attached, not door-internal; FUSE as one projection
- `AGENTS.md`: r9p core invariants and the backend-neutral boundary test
- `docs/source-map.md`: current r9p source map
- `crates/fuse/src/p9.rs`: current generic 9P client wrapper, request tracker, address parsing, retry, and error mapping
- `crates/fuse/src/fuse/mod.rs`: `ClientSlot`, initial attach, root stat, mount run state
- `crates/fuse/src/fuse/dispatch.rs`: reconnect, FUSE interrupt routing, namespace shape recovery
- `crates/fuse/src/fuse/change_feed.rs`: separate feed client and current polling consumer
- `crates/fuse/src/node.rs` and `crates/fuse/src/node/handles.rs`: path/qid/stat/fid cache and FUSE handle ownership
- `crates/cli/src/commands/mount.rs`: existing mount lifecycle verbs and systemd user-unit wrapper

Findings:

- The reusable session boundary is already visible in `crates/fuse/src/p9.rs`: endpoint parsing, TCP or namespace Unix socket dialing, attach, multiplexed calls, request timeout wrappers, reconnect retry, and remote-error-to-errno mapping do not depend on the Linux FUSE ABI.
- FUSE interrupt support currently uses the same request tracker but keys active calls by FUSE `unique`. That can move with the generic client as an opaque operation-tracking mechanism for slice 1, while the FUSE edge remains the only caller that sets FUSE `unique`.
- `ClientSlot` and reconnect transport re-establishment can move toward session ownership, but path rebind recovery still depends on the node/path table. Plan 83's Claude amendment is correct: transport reconnect is slice 1; rebind recovery belongs with the shared cache/path-table slice.
- Node ids, lookup counts, FUSE generations, open handle tables, `READDIRPLUS` per-open directory snapshots, kernel invalidations, errno replies, and mount lifecycle are FUSE-edge responsibilities and should not move into `crates/session` in slice 1.
- The local control namespace and session-hosted FUSE projection require a future daemon process. Slice 1 should not create that daemon yet; it should create the crate boundary and make FUSE consume the shared client layer unchanged.

Effect on code:

- Add a new `crates/session` crate that depends on `r9p` and `libc`, not on `fuse`.
- Move the current generic 9P client wrapper, address parsing, request tracker, and error mapping from `crates/fuse/src/p9.rs` into `crates/session`.
- Move the generic current-client slot into `crates/session/src/slot.rs` so FUSE can share client snapshot and replacement mechanics without owning the lock wrapper. Keep path rebind recovery in FUSE until the path table boundary moves further, because it still depends on FUSE node ids and stale-binding handling.
- Keep `crates/fuse` behavior unchanged by importing `session::{Client, OREAD, OWRITE, ORDWR, OTRUNC, with_fuse_unique}` and converting `session::Error` into the FUSE error type.
- Leave the FUSE node table, handle table, reconnect rebind, change-feed invalidation, and mount lifecycle in `crates/fuse` for later Plan 83 slices.

Open questions:

- Slice 2 should decide whether the path/qid/stat directory cache becomes a generic session cache or a smaller shared cache type consumed by FUSE.
- Slice 3 should decide the exact local 9P control namespace file layout, including `usage`, `status`, `freshness`, `diagnostics`, and `query`.
- Slice 6 should replace the current change-feed polling loop with a blocking stream read while preserving degraded feed markers and cancellation behavior.
