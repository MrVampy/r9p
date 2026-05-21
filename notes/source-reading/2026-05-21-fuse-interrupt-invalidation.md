# 2026-05-21 FUSE Interrupt And Invalidation

## Question

How should `r9p mount` harden Linux FUSE requests that hang, get interrupted,
or outlive namespace cache state?

## Files Inspected

- `crates/core/src/multiplex/client.rs`
- `crates/fuse/src/fuse/dispatch.rs`
- `crates/fuse/src/fuse/reply.rs`
- `crates/fuse/src/fuse/wire.rs`
- `refs/linux-fuse/include/uapi/linux/fuse.h`
- `refs/linux-fuse/fs/fuse/dev.c`
- `refs/libfuse/lib/fuse_lowlevel.c`
- `refs/plan9port/src/lib9p/srv.c`
- `refs/9front/sys/src/lib9p/srv.c`

## Findings

- Linux FUSE sends `FUSE_INTERRUPT` with a `fuse_interrupt_in.unique` pointing
  at the original kernel request. libfuse treats interrupt requests specially:
  normal operations can have an interrupt record attached, and the interrupt is
  not a normal operation reply path.
- Plan 9's protocol primitive for canceling an outstanding tagged request is
  `Tflush oldtag`, with `Rflush` confirming the flush request. plan9port and
  9front server code both treat flush as the protocol-level way to release work
  associated with an old tag.
- Linux FUSE exposes explicit cache notifications with
  `FUSE_NOTIFY_INVAL_INODE` and `FUSE_NOTIFY_INVAL_ENTRY`. libfuse writes these
  as out headers with `unique = 0` and `error = notify_code`.

## Effect

- `MultiplexedClient` now sends a bounded `Tflush` when a timed-out 9P call
  gives up waiting for its response.
- `r9p mount` tracks the FUSE request `unique` around worker dispatch, maps it
  to submitted 9P tags, and translates `FUSE_INTERRUPT` into `Tflush` for the
  active backing tag.
- Namespace-control writes keep the existing stale-fid marking, and now also
  send FUSE invalidation notifications for affected node and dentry cache
  state.

## Open Questions

- The ignored host-gated stress test covers recursive reads over a real FUSE
  mount. A future CI host with `/dev/fuse` access could run it by default.
