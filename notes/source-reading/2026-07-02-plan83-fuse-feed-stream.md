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

## Findings

- The runtime must still see FUSE as an ordinary attached 9P client. No core namespace path or `/srv` registration is needed for this slice.
- Stream and poll cannot share cursor rules. A stream fid owns its cursor and delivered records are processed directly; a poll read must still select records against the last observed event id unless a since-template path is configured.
- FUSE should reuse the generic session feed record parser and cursor selector, but keep event application local because kernel invalidation, stale bindings, and node-table rebinds are FUSE projection state.
- Status needs a feed source field. Without it, a mount using stream-primary could silently fall back to poll and still report only `change_feed: "connected"`.

## Effect

- Add `change_feed_stream_path` to the FUSE mount config.
- Add `r9p mount --change-feed-stream PATH`, matching the existing `r9p session serve --change-feed-stream PATH` vocabulary.
- Refactor the FUSE change-feed loop so stream consumption is primary and poll is fallback.
- Add `source: "stream"` or `source: "poll"` to the FUSE status JSON.
- Keep FUSE node state, open handles, stale binding clunks, and kernel invalidation in `crates/fuse`.

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

## Open Questions

- A later Plan 83 slice still needs to host FUSE inside the long-lived `r9p session` process when shared cache across session queries and FUSE is required.
- A future live proof should trigger a real namespace mutation while FUSE is blocked on `/events/namespace/stream`, proving kernel invalidation from a fresh stream event rather than only stream attachment and shallow reads.
