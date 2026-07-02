# Plan 83 Query Contract Slice

Date: 2026-07-02.

Question: how much of the Plan 83 namespace-native snapshot/query contract can be implemented now without making the control tree or FUSE adapter own query semantics?

## Sources Checked

- Vault `docs/plan/83/index.md`: slice 5 requires path, depth, include files or dirs, requested stat fields, entry or branch budget, freshness mode, and explicit degraded markers.
- `notes/source-reading/2026-07-02-plan83-control-surface.md`: the local control socket is a 9P namespace, with `/query` as the parameterized JSON RPC file.
- `crates/session/src/control/tree.rs`: owns the local 9P control namespace and should stay a dispatcher, not a query parser.
- `crates/session/src/control/query.rs`: owns JSON query parsing and response routing.
- `crates/session/src/control/snapshot.rs`: owns subtree walking and snapshot/list/stat/read JSON rendering.
- `crates/session/src/cache.rs`: current freshness helpers are generic, but no feed cursor or session epoch is wired into the control server yet.
- `crates/cli/tests/session_control.rs`: synthetic 9P server proof for the local control socket and direct `/query` RPC.

## Findings

- The next correct slice is to enrich `/query`, not the path-shaped control files. The path files remain a simple inspection surface; `/query` carries parameterized agent requests.
- Include filtering, entry field selection, and entry budgets can be implemented entirely in `crates/session/src/control/` without touching FUSE or r9p core.
- Freshness barrier modes need session epoch and namespace change-feed state. Implementing `sync` or `max_age` before that state exists would create a dishonest contract, so this slice leaves freshness modes for the change-feed/session-state slice.
- The option vocabulary deserves its own owner file. Adding it to `tree.rs` would mix 9P namespace dispatch with query policy; adding it to FUSE would violate Plan 83's projection boundary.

## Effect

- Add `crates/session/src/control/options.rs` for snapshot include, field selection, and entry budget options.
- Keep `ControlRequest` as the simple CLI/path request type, and introduce a private `QueryRequest` for `/query` so the richer JSON contract does not leak into the public path-control type.
- Extend snapshot rendering with `mtime`, optional entry fields, `include` filtering, and explicit `budget_truncated` degradation when the entry budget is exhausted.
- Prove the contract through direct `r9p rpc /query` against the local Unix-socket 9P control namespace.

## Open Questions

- Wire `session_epoch`, feed generation, cache age, and `fresh_instance` once the session owner consumes `/events/namespace`.
- Decide whether the default snapshot entry shape should keep `length` as the 9P field name or move to a separate higher-level `size` field in a future versioned response.
