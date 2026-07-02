# Plan 83 Session Epoch Reconnect Slice

Date: 2026-07-02.

Question: how should a session-hosted FUSE reconnect update the session freshness identity seen by the local control namespace?

## Sources Checked

- Vault `docs/plan/83/index.md`: the snapshot/query contract requires `session_epoch` to bump on daemon start and on every reconnect, because reconnect does not preserve fid-layer state.
- `notes/source-reading/2026-07-02-plan83-session-hosted-fuse.md`: the hosted FUSE slice left one open question, namely that FUSE reconnect updated the shared client slot without bumping the session epoch.
- `crates/session/src/slot.rs`: the shared `ClientSlot` is the one object both control queries and hosted FUSE use to see the current door client.
- `crates/fuse/src/fuse/dispatch.rs`: FUSE reconnect replaces the shared `ClientSlot` after rebinding path-backed FUSE nodes.
- `crates/session/src/control/{mod,tree,query,freshness}.rs`: control responses attach the current `session_epoch` and feed freshness to status/query/snapshot/list/stat/read responses.

## Findings

- The epoch belongs with the shared session slot, not with FUSE or the control tree. Any client replacement means the fid layer changed, regardless of which projection triggered it.
- Passing epoch strings into control connection trees freezes the epoch for the lifetime of that local 9P connection. The control tree must read the shared epoch at response time.
- Standalone `r9p mount` can keep an unobserved slot epoch; the session-hosted path shares the epoch with the control namespace through `ControlRuntime`.

## Effect

- Add `SessionEpoch` to `crates/session`.
- Make `ClientSlot::replace` bump the shared epoch after swapping the current client.
- Make `ControlRuntime` create the epoch and pass a clone into the shared `ClientSlot`.
- Make control status/query/snapshot/list/stat/read responses read `SessionEpoch` at response time.

## Proof

- `cargo check -p session`
- `cargo fmt --all --check`
- `cargo check -p cli`
- `cargo test -p session`
- `cargo test -p fuse`
- `cargo test -p cli --test session_hosted_fuse -- --ignored --test-threads=1`

## Open Questions

- The control query contract still does not accept a caller-presented prior epoch, so it cannot yet compute a per-caller `fresh_instance` flag from an epoch mismatch. It now reports the correct changed epoch after reconnect; a later contract slice can add presented-epoch comparison without touching FUSE reconnect again.
