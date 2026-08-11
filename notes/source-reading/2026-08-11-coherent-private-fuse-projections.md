# Coherent Private FUSE Projections

## Question

How should a short-lived application launcher expose an admitted read-only 9P
subtree as a native filesystem without turning the application supervisor into
a privileged mount owner or making stale kernel cache entries authoritative?

## Sources inspected

- `crates/fuse/src/fuse/dispatch.rs`
- `crates/fuse/src/fuse/change_feed.rs`
- `crates/fuse/src/fuse/mod.rs`
- `crates/fuse/src/fuse/mount.rs`
- `crates/fuse/src/fuse/ops/io/open.rs`
- `crates/session/src/feed/event.rs`
- `refs/coordinator/refs/libfuse/include/fuse_kernel.h`
- `refs/coordinator/refs/libfuse/example/notify_inval_inode.c`
- `refs/coordinator/refs/libfuse/lib/fuse_loop.c`

## Findings

Linux distinguishes cache policy at open time. `FOPEN_DIRECT_IO` bypasses the
page cache, while `FOPEN_KEEP_CACHE` preserves it across opens. Libfuse's
invalidation example combines retained cache contents with explicit inode
notifications; retained cache without an invalidation source is not coherent.

The r9p bridge already translates generic namespace-change records into kernel
inode and entry invalidations. It can therefore retain read cache only when a
mandatory blocking change feed is configured. Files without authoritative
length information and non-read-only opens remain direct I/O.

Mount readiness and mount lifetime are different concerns. A launcher needs to
know when the kernel mount exists before it executes a child, then retain a
handle that unmounts and joins the FUSE loop when the child ends. For several
mounts, every `/dev/fuse` descriptor must be acquired before the first runtime
thread starts. Each managed thread then drops all capabilities before opening
its 9P client, so neither the client reader thread nor later mount preparation
inherits `CAP_SYS_ADMIN`.

Stopping a managed mount must wake blocking event consumers. A periodic
receive timeout is not an event-driven stop mechanism, and a long blocking
feed read must not delay unmount. The session-feed receiver now has an explicit
wake handle, while a direct feed shutdown closes its client transport and wakes
its reconnect condition variable.

Normal filesystem reads and blocking feed reads also need separate deadlines.
The former retain the finite request budget; only the declared blocking feed
uses the longer feed-read budget.

## Effect

- `r9p_fuse::start` returns a managed mount handle after mount readiness, and
  `start_all` safely prepares several mounts as one batch.
- Coherent read cache is opt-in and requires a namespace change feed.
- Change-feed shutdown is event-driven and interrupts blocking reads.
- Data-read and change-feed-read timeouts are independent.
- Application-specific path meaning and launch policy remain outside r9p.

## Open questions

None for the read-only Agent filesystem projection. Writable coherent mounts
would require an independently reviewed mutation and conflict contract.
