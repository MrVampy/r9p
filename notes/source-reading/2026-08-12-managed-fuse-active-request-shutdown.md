# Managed FUSE shutdown with active requests

Question: why could a managed Agent filesystem still strand its launcher after
the private shutdown wake and auto-unmount guardian fixes were deployed?

Sources inspected:

- `crates/fuse/src/fuse/dispatch.rs`: managed shutdown ordering, change-feed
  shutdown, and worker joins.
- `crates/fuse/src/fuse/change_feed.rs`: direct-feed cancellation and kernel
  cache invalidation writes.
- `crates/fuse/src/fuse/mount.rs`: FUSE descriptor custody, connection abort,
  guardian shutdown, and lazy unmount.
- `crates/fuse/src/fuse/mod.rs`: `MountHandle` and `FuseMount` lifetimes.
- Linux process evidence from M7 run
  `memory-d95cf6708c2b8bd10d2ffe0dc24b401f-8`.

Finding:

The private shutdown descriptor woke the managed `/dev/fuse` dispatch loop,
but the loop then joined the change-feed and request workers before the FUSE
connection was aborted. A provider background search had left active kernel
requests while a concurrent change-feed invalidation was blocked in
`folio_wait_bit_common`. The provider had already emitted its valid structured
result, but the launcher remained in `MountHandle::drop` because the active
FUSE work could not finish while the connection remained live.

Live recovery confirmed the boundary: terminating the orphaned read-only
search and lazily detaching the path did not release the launcher. Writing to
the owner-accessible `/sys/fs/fuse/connections/<id>/abort` endpoint immediately
woke the blocked FUSE threads, completed mount teardown, and allowed Agent to
publish the already produced result and release its credential.

Decision:

Managed shutdown closes and aborts the owned FUSE connection before joining
the change-feed and request workers. The ordinary `FuseMount` drop remains the
idempotent final cleanup owner. Connection abort also precedes waiting for the
auto-unmount guardian, so guardian completion cannot depend on kernel requests
that only the later abort would release.

This remains FUSE lifecycle behavior in `crates/fuse`. Agent and the 9P core do
not gain provider-specific cleanup or process policy.
