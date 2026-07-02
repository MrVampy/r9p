# Plan 83 Session-Hosted FUSE Slice

Date: 2026-07-02.

Question: how should `r9p session serve` host a FUSE projection without making FUSE a runtime service or giving it a second namespace-change feed cursor?

## Sources Checked

- Vault `docs/plan/83/index.md`: the process topology pin says one daemon process should host the control socket and optional FUSE projection so they share one attachment, cache owner, and change-feed cursor.
- Vault `docs/architecture/61-door-attached-namespace-sessions.md`: the session manager is door-attached, not door-internal, and remains local access infrastructure rather than a participant service.
- `crates/session/src/control/mod.rs`: the control socket owner already creates the long-lived client, feed state, namespace cache, and feed worker.
- `crates/session/src/feed/worker.rs`: the feed worker owns stream-primary and poll-fallback cursor selection and already invalidates the session cache from generic namespace-change records.
- `crates/fuse/src/fuse/change_feed.rs`: standalone FUSE owns kernel invalidation, FUSE node cache invalidation, stale fid clunks, and mount status for namespace-change records.
- `crates/fuse/src/fuse/dispatch.rs`: the FUSE event loop starts the FUSE worker pool and currently starts its own feed consumer.
- `crates/fuse/src/fuse/mod.rs`: `R9pFuse` owns the FUSE-local `NodeTable`, `ClientSlot`, status, diagnostics, and mount lifecycle.
- `crates/cli/src/commands/session.rs` and `crates/cli/src/commands/mount.rs`: the CLI already separates session control verbs from standalone mount lifecycle verbs.

## Findings

- Sharing cloned `Client` handles is not a sufficient session topology. FUSE reconnect can replace its client; control queries and feed consumption need to see the same current client slot.
- The generic feed worker should not learn FUSE. It can publish generic feed events on an in-process bounded bus; FUSE remains the only owner of kernel invalidation and FUSE node-table invalidation.
- Session-hosted FUSE must not open `/events/namespace/stream` itself. It should subscribe to the session feed worker's parsed event stream, so the runtime sees one feed cursor for the session process.
- The standalone `r9p mount` path remains valid and keeps its own feed loop for session-less use.
- The session command should not grow all standalone mount supervision machinery. The first hosted projection only needs the mountpoint plus minimal mount status, diagnostics, and TTL options needed for proof.

## Effect

- Add a generic `FeedEventBus` in `crates/session::feed` carrying `NamespaceChange` and coarse-invalidation events.
- Make the session control runtime own a shared `ClientSlot`, `NamespaceCache`, `FeedState`, feed event bus, and feed worker handle.
- Make control 9P requests snapshot the shared `ClientSlot` per request instead of holding a detached `Client`.
- Add `fuse::mount_with_session(config, client_slot, feed_receiver)` for session-hosted projections while leaving `fuse::mount(config)` as the standalone path.
- Add `r9p session serve --mount MOUNTPOINT` plus small mount projection options under a focused CLI module.
- Add a host-gated proof that session-hosted FUSE and the control socket share one session feed stream: the synthetic server sees exactly one stream reader while a FUSE mount receives invalidation from a `r9p write /mutate` event.

## Proof

- `cargo fmt --all --check`
- `cargo check -p cli`
- `cargo test -p session`
- `cargo test -p fuse`
- `cargo test -p cli`
- `cargo test -p cli --test session_hosted_fuse -- --ignored --test-threads=1`
- `cargo test -p cli --test fuse_stream_invalidation -- --ignored --test-threads=1`
- `cargo test -p cli --test fuse_mount -- --ignored --test-threads=1`

## Open Questions

- Shared reconnect epoch handling is still incomplete: FUSE reconnect now updates the shared client slot, but the session epoch is not bumped yet.
- The first hosted mount CLI intentionally has a small option surface. If hosted FUSE adoption becomes routine, a later slice should decide which standalone mount lifecycle options deserve a shared parser rather than duplicating flags.
