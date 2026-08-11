# Managed FUSE shutdown wake

Question: why can an Agent headless run remain active after its native harness
has exited when the run projects a namespace filesystem?

Sources inspected:

- `crates/fuse/src/fuse/mod.rs`: managed mount startup, `MountHandle` drop, and
  mount-thread joining.
- `crates/fuse/src/fuse/mount.rs`: FUSE descriptor custody, lazy unmount, and
  connection abort cleanup.
- `crates/fuse/src/fuse/dispatch.rs`: the blocking `/dev/fuse` read loop and
  worker/change-feed shutdown.
- `crates/session/src/projection/mod.rs`: the existing private Unix-stream wake
  used to stop a blocking namespace projection independently of its public
  socket.
- Linux process evidence from an affected M7 Agent run: the provider child had
  exited, the launcher main thread waited on a join futex, and the
  `r9p-fuse-mount` thread remained in `fuse_dev_do_read`.

Finding:

`MountHandle::drop` lazily detached the mount and immediately joined the mount
thread. Detaching the path did not wake the thread's blocking read on the FUSE
device, so the join could wait forever. The launcher therefore never reported
the provider result or released its credential even though the provider
process was already gone.

Decision:

Each managed mount owns a private Unix-stream wake pair. The mount thread polls
the FUSE device and its private wake descriptor together. Dropping the handle
signals that descriptor, lets the event loop stop its change feed and worker
pool, and only then joins the thread. The mount thread retains ordinary FUSE
descriptor and unmount cleanup custody.

This belongs in the FUSE runtime adapter, not Agent and not the 9P protocol
core. It is the same lifecycle principle already used by namespace projection:
internal shutdown must not depend on an external pathname operation waking a
blocking descriptor.
