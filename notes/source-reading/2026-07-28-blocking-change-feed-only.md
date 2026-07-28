# Blocking Change Feeds Without Polling

Date: 2026-07-28

## Question

Can r9p's namespace-change consumers use blocking 9P reads as the sole live
notification mechanism, while retaining cursor recovery after a disconnect?

## Sources Inspected

- `crates/session/src/feed/worker.rs`
- `crates/session/src/feed/event.rs`
- `crates/session/src/control/mod.rs`
- `crates/fuse/src/fuse/change_feed.rs`
- `crates/fuse/src/fuse/config.rs`
- `crates/cli/src/commands/session.rs`
- `crates/cli/src/commands/mount/config.rs`
- `../r9wm/crates/terminal/client/src/feed.rs`

## Findings

The shared session worker already preferred a blocking stream, and r9wm already
configured one. However, the stream was optional. Without it, the worker
reopened and reread the recent or cursor path at a fixed interval.

The standalone FUSE feed had the same fallback. Even with a stream configured,
it returned to a periodic snapshot read after a stream error. The recent and
cursor paths were therefore serving two different purposes: recovery from a
known event cursor and ordinary change discovery.

The state owner already exposes the correct live signal as a blocking 9P read.
The snapshot path is still useful, but only once after a disconnect to recover
records newer than the last delivered event ID. Connection and read deadlines
remain valid liveness and cleanup bounds. A delay after a failed reconnect is
backoff, not a discovery cadence.

## Effect

The feed worker now requires a blocking stream path. The recent or
cursor-addressed path is used only for one-shot catch-up after a stream break.
The standalone FUSE adapter follows the same contract. Configuration names say
`reconnect_delay`, and the CLI exposes `--change-feed-reconnect-delay`; the
polling option and fallback are removed forward-only.

r9wm changes only its construction of the shared r9p feed configuration. It
does not gain a terminal-specific subscription mechanism.

## Open Questions

The blocking reads still use finite read deadlines so shutdown and broken peers
cannot strand a worker forever. If r9p later exposes cancellable retained reads
to these adapters, `Tflush` can make shutdown immediate without changing the
stream-primary contract.
