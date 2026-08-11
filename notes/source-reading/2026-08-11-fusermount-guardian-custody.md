# Fusermount Guardian Custody

Date: 2026-08-11

## Question

Why can a managed FUSE projection finish its event loop while the embedding process and its
systemd cgroup remain alive, and which layer should end the remaining process?

## Sources Inspected

- `crates/fuse/src/fuse/mount.rs`: `mount_fuse`, `mount_fuse_attempt`, `FuseMount::drop`, and
  `MountCleanup`.
- `crates/fuse/src/fuse/mod.rs`: `MountHandle::stop_inner`, `R9pFuse::mount_managed`, and the
  ownership boundary between the managed event-loop thread and `FuseMount`.
- libfuse commit `20c0087ad327c64e3ad711706ca585c421571963`,
  `util/fusermount.c`: `main`, `wait_for_auto_unmount`, and `should_auto_unmount`.

## Findings

With `auto_unmount`, `fusermount3` does not exit after sending the `/dev/fuse` descriptor.
It deliberately remains as a guardian and blocks in `recv` on the communication socket. EOF on
that socket means the mounting process has ended, so the guardian checks whether the FUSE mount
is disconnected and unmounts it when necessary.

The r9p bridge previously converted its end of this socket to `File` and then forgot the value.
That provided crash cleanup, but it erased normal-lifecycle ownership. A managed mount could wake
and finish its FUSE thread while the forgotten socket still kept `fusermount3` alive. An embedding
supervisor that correctly waits for the worker cgroup to empty before releasing credentials then
waits forever: the launcher waits for release, while the guardian waits for the launcher-owned
socket to close.

The socket is therefore not an implementation detail that may be leaked. It and the guardian pid
are one owned lifecycle object. Managed teardown must close the socket, wait for the guardian to
exit, and only then report the mount stopped. The socket is also marked close-on-exec so an
unrelated descendant cannot accidentally extend the guardian lifetime.

This remains FUSE adapter mechanism. Agent, Terminal, Memory, Coordinator, and the reusable 9P
protocol core do not need service-specific lifecycle logic.

## Effect On r9p

`MountCleanup` now owns an optional auto-unmount guardian. Its normal and signal teardown paths
take that guardian exactly once, close the liveness socket, and reap the helper. The fallback path
for helpers without `auto_unmount` remains a synchronous helper invocation with no resident
guardian.

A focused regression forks a syscall-only helper that waits for EOF. The test proves that dropping
the owned guard produces EOF and that teardown reaps the helper before returning.

## Open Questions

None for the managed-mount lifecycle. The live embedding proof still needs to show the worker
cgroup emptying and the credential release completing after this exact revision is deployed.
