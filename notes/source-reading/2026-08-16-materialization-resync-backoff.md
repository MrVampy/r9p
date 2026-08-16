# Materialization Resynchronization Backoff

## Question

Can a failed coherent local materialization enter a hot retry loop when its
change feed continues to wake while the source subtree remains unavailable?

## Sources Inspected

- `crates/session/src/materialization.rs`, especially `event_loop`.
- `crates/session/src/feed/event.rs`, especially `FeedWake::wait_after`.
- `crates/session/src/feed/worker.rs`, especially feed reconnect and shutdown.
- `crates/reverse/src/export.rs`, especially `retry_delay` and
  `ExportState::wait_before_retry`.
- The live M7 Agent journal while its configured Memory service was stopped.
- The kube-rs watcher recovery documentation and gRPC connection backoff
  protocol.

## Findings

M7 emitted 17,653 identical local materialization failures in one minute. The
materialization resynchronization failure path observed a feed generation and
then called `FeedWake::wait_after`. A reconnecting feed kept advancing that
generation even though the source subtree still did not exist, so every wait
returned immediately and the snapshot retried without a bound.

The wake generation means "new feed activity", not "the source is now
readable". It is therefore not sufficient recovery evidence after a concrete
snapshot failure. The shared reverse-export runtime already uses the right
local pattern: an interruptible retry deadline with a bounded increasing
delay. kube-rs likewise applies backoff around a recovering watcher, and the
gRPC connection backoff protocol requires an initial delay, multiplication up
to a maximum, and reset after successful recovery.

## Effect on r9p

The materialization worker now waits for a retry deadline that feed-change
notifications cannot bypass. The delay starts at one second, doubles to a
maximum of sixty seconds, and resets after a coherent snapshot succeeds. Feed
shutdown still interrupts the wait immediately, so teardown does not become a
timer-bound operation. A regression floods the wake generation and proves
that the retry deadline remains in force.

This remains in the generic session materialization layer. Agent, FUSE, and
future consumers receive the same recovery behavior without application-side
retry loops.

## Open Questions

None for this defect. A future workload with many independent materializations
may justify per-worker jitter, but one bounded worker per materialized subtree
does not require an additional random dependency for this repair.
