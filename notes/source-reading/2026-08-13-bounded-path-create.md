# Bounded path creation

Date: 2026-08-13

Question: Where should a deadline-aware native namespace create used by r9wm
live?

Files and functions inspected:

- `crates/session/src/client/paths.rs`: `Client::create_at`,
  `finish_fid_timeout`, and path validation.
- `crates/session/src/client/namespace/operations.rs`:
  `Client::create_timeout` and referral-boundary enforcement.
- `crates/fuse/src/fuse/ops/create.rs`: bounded native create and the rule
  against replay after an ambiguous mutation result.
- `crates/core/src/server/mod.rs`: the generic `FileTree::create` operation.

Findings:

- r9p already owns native `Tcreate`, referral routing, per-operation deadlines,
  and fid cleanup.
- The path facade exposed unbounded `create_at`, but not the equivalent bounded
  composition. Reimplementing walk, create, and clunk in r9wm would duplicate a
  generally useful r9p concern.
- A timed-out mutation cannot be replayed safely. The bounded helper must return
  the ambiguous failure after cleanup rather than issue a second create.

Effect:

- Add `Client::create_at_timeout` to the session path facade.
- r9wm can create a Terminal session with ordinary namespace semantics while
  retaining a bounded launch path and no terminal-specific 9P machinery.

Open questions: none.
