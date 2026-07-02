# Plan 83 Session Feed Status Slice

Date: 2026-07-02.

Question: how should the session control surface start consuming the namespace change feed without turning FUSE into the owner of feed state or claiming freshness modes that are not implemented yet?

## Sources Checked

- Vault `docs/plan/83/index.md`: the M7 runtime already serves `/events/namespace/{stream,recent,status}` and `/events/namespace/since/<event_id>`; Plan 83 says consume it, do not rebuild it.
- Vault `src/core/api/namespace/source/events/runtime.gleam`: `/events/namespace/status` advertises the raw recent path, stream path, since template, and generation bounds.
- `crates/session/src/feed/record.rs`: shared parser/cursor utilities from the prior slice.
- `crates/session/src/control/mod.rs`, `server.rs`, `tree.rs`, and `query.rs`: the local Unix-socket 9P control namespace and status/query response routing.
- `crates/cli/src/commands/session.rs`: `r9p session serve` argument parsing and control socket startup.
- `crates/cli/tests/session_control.rs`: synthetic 9P server proof for session control.

## Findings

- The session owner can start tracking feed cursor state before it owns cache invalidation. That gives status/freshness evidence for later snapshot contracts without moving FUSE kernel invalidation or node-table behavior into session.
- Feed state needs its own owner separate from record parsing. `feed.rs` was split into `feed/record.rs`, `feed/state.rs`, and `feed/worker.rs` before adding the worker to avoid turning the feed module into a new mixed-purpose file.
- The first worker should be status-only: parse records, advance cursor evidence, mark cursor misses as `fresh_instance`, and report degraded/backpressure states. Cache invalidation remains with whichever projection owns cache state.
- `r9p session serve` can accept the same conceptual feed options as `r9p mount`: feed path, cursor template, poll interval, and backpressure limit. The endpoint positional argument must be parsed after stripping those flags.

## Effect

- Add `FeedState` and a session feed worker under `crates/session/src/feed/`.
- Extend `session.status.v1` with a `feed` object containing state, last event id, last generation, fresh-instance flag, and last error.
- Add `r9p session serve --change-feed PATH` flags.
- Extend the session-control integration fixture with `/events/namespace/recent` and prove status reaches `last_event_id="e1"` and `last_generation=42`.

## Live Proof

After committing the code slice, a temporary local session manager was started against M7:

```bash
target/debug/r9p -a 192.168.0.30:9564 -u codex -A / -m 65536 session serve --socket "$sock" --change-feed /events/namespace/recent --change-feed-cursor-template '/events/namespace/since/{event_id}' --change-feed-poll-interval 0.1
```

`target/debug/r9p session status --socket "$sock"` returned `session.status.v1` with:

- `endpoint`: `192.168.0.30:9564`
- `msize`: `65536`
- `feed.state`: `connected`
- `feed.last_event_id`: `5kvwlr7caxtx46gy5p7urvncgdp5dknl:0000000000000009`
- `feed.last_generation`: `881388`
- `feed.last_error`: `null`

## Open Questions

- The worker still uses poll/read cycles. The next slice should use the blocking `/events/namespace/stream` posture as primary and keep `since/<id>` or recent polling as degraded fallback.
- Snapshot freshness modes should wait until session epoch, feed generation, cache age, and sync barrier behavior are all wired together.
