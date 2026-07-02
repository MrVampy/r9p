# Plan 83 Feed Stream Primary Slice

Date: 2026-07-02.

Question: how should the session feed worker prefer the runtime namespace stream while keeping recent/since polling as the degraded fallback?

## Sources Checked

- Vault `docs/plan/83/index.md`: Plan 83 correction says `/events/namespace/{stream,recent,status}` and `/events/namespace/since/<event_id>` already exist, and the shared session should move to blocking reads on `/events/namespace/stream` with polling as fallback.
- Vault `docs/plan/54/index.md`: stream files have per-fid cursor state; empty successful reads mean no records; stale/malformed/foreign cursors must surface via the read error path or explicit degraded record, never hidden metadata.
- Vault `src/core/api/namespace/source/events/runtime.gleam`: `/events/namespace/stream` and `/events/namespace/recent` are distinct namespace files under the events projection and share the current namespace-change content producer.
- `crates/session/src/feed/worker.rs`: existing session feed worker polled a recent/since path and selected records by remembered event id.
- `crates/cli/tests/session_control.rs`: synthetic server proof for session control and feed status.

## Findings

- Stream and poll processing must not be the same function with the same cursor rule. A stream fid owns its cursor; the worker should process delivered stream records directly. A recent/since fallback does not own a per-fid cursor across requests, so it needs explicit record selection against the remembered event id.
- The correct v1 CLI is explicit: `--change-feed-stream PATH` names the stream path. Deriving `/stream` from `/recent` would bake Vault path conventions into generic r9p code.
- The feed worker still uses bounded read timeouts so stop/retry behavior remains finite. A stream timeout is not a data-client-stale condition; it keeps status connected and loops.
- Fallback remains available: if stream consumption errors, the worker marks the feed degraded and then tries the configured recent/since path.
- Status needs to expose the active feed source so stream-primary proofs are evidence-based. A connected feed can be `source: "stream"` or `source: "poll"`; otherwise a missing stream path could silently fall through to poll and still look healthy.

## Effect

- Add `FeedWorkerConfig.stream_path`.
- Add `r9p session serve --change-feed-stream PATH`.
- Split worker processing into stream mode and poll mode: stream mode processes all delivered records; poll mode uses `select_feed_records`.
- Add `feed.source` to session status so agents can distinguish the stream owner from poll fallback.
- Extend the synthetic integration fixture with `/events/namespace/stream` and pass `--change-feed-stream` through the real CLI.

## Live Proof

- Local proof gate: `cargo fmt --all --check`, `cargo test -p session`, `cargo test -p cli --test session_control`, `cargo test -p fuse`, `cargo test -p cli --test cli_machine`, `cargo check -p cli`, and `git diff --check`.
- M7 proof against `192.168.0.30:9564`: a temporary local session owner was started with `--change-feed /events/namespace/recent`, `--change-feed-stream /events/namespace/stream`, and `--change-feed-cursor-template '/events/namespace/since/{event_id}'`.
- The status query returned `feed.state: "connected"`, `feed.source: "stream"`, `feed.last_generation: 881388`, and `feed.last_event_id: "5kvwlr7caxtx46gy5p7urvncgdp5dknl:0000000000000009"`.

## Open Questions

- A future live proof should trigger a real namespace mutation while the session worker is blocked on `/events/namespace/stream`, proving wakeup without relying on recent-window content at attach.
- FUSE still has its separate polling consumer. Moving FUSE onto the shared session feed/cache owner is a later Plan 83 adoption slice.
