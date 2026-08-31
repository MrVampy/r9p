# FUSE Mount Generation Continuity

Date: 2026-08-31

## Question

How should a NixOS activation install a new r9p FUSE mount implementation
without invalidating file descriptors held by applications on the active
mount?

## Sources Inspected

- Linux kernel `Documentation/filesystems/fuse/fuse.rst`, connection lifetime,
  lazy detach, control filesystem, and daemon-failure behavior.
- `refs/libfuse/include/fuse_lowlevel.h` and
  `refs/libfuse/lib/fuse_signals.c`, session exit, unmount, destroy, and signal
  handling.
- NixOS `nixos/lib/systemd-lib.nix` and
  `nixos/doc/manual/development/unit-handling.section.md`,
  `reloadIfChanged`, `restartIfChanged`, and `stopIfChanged` activation
  semantics.
- systemd service documentation for `ExecReload` and `Restart` behavior.
- r9p `crates/fuse/src/fuse/mount.rs`, signal-owned teardown and FUSE FD
  custody.
- r9p `crates/fuse/src/node/handles.rs`, open FUSE handle ownership.
- r9p `crates/fuse/src/fuse/status.rs`, local mount observability.
- JuiceFS `cmd/mount_unix.go`, `cmd/passfd.go`, and
  `docs/en/administration/upgrade.md`, smooth binary upgrade through a stable
  parent, SCM_RIGHTS FUSE FD transfer, and saved worker state.
- JuiceFS CSI `pkg/fuse/passfd/`, `pkg/fuse/grace/`, and the Linux Foundation
  talk "Enabling Seamless AI Workloads: Achieving Zero-Downtime Upgrades for
  FUSE in Kubernetes".
- containerd stargz-snapshotter issue 318, separate FUSE-manager and FUSE-FD
  transfer proposals after process restarts invalidated containers.

## Findings

The FUSE kernel connection belongs to the daemon's `/dev/fuse` file
descriptor. Killing that daemon or closing the descriptor cannot preserve
application file handles. Recreating the mount at the same path helps only
later path resolution; already-open operations still fail.

NixOS already supports the required activation distinction. A changed service
with `reloadIfChanged` is reloaded rather than stopped and started. The daemon
must define reload as a service-owned lifecycle transition.

JuiceFS demonstrates immediate mid-session binary replacement by retaining the
FUSE FD in a stable parent and transferring it, plus serialized worker state,
to the new process. r9p would additionally need to serialize node IDs, qids,
paths, 9P bindings, open handles, directory cursors, and in-flight request
state. Writable and RPC handles cannot be reconstructed by guessing about
delivery.

Linux lazy detach gives r9p a smaller protocol-correct boundary. Detaching the
old mount removes it from fresh pathname resolution without closing its FUSE
FD. Existing open files, directories, and working-directory references keep
the detached superblock alive and continue sending requests to the old daemon.
A successor can mount at the same pathname immediately. The kernel sends
`FUSE_DESTROY` to the detached generation only after its final reference is
gone, so r9p does not have to infer liveness from a partial userspace handle
count.

## Effect

The systemd reload command starts the new r9p binary in two phases. It first
prepares the namespace connection and source binding. Only after that preflight
succeeds does it signal the active generation to lazy-detach, start the
successor mount, publish successor readiness and main-PID adoption, wait on the
systemd notification barrier, and release the retired generation's replacement
fence. The active PID is supplied by systemd's reload command; r9p does not
query host service state through an ambient helper. SIGINT and SIGTERM retain
immediate owned teardown.

The old generation stops publishing shared status when it retires but keeps
serving its detached FUSE connection until the kernel destroys it. The new
generation owns the pathname and shared status immediately. No request, file
handle, node ID, directory cursor, or 9P operation crosses a process boundary,
and no operation is replayed.

If a future consumer requires a single connection to change workers rather
than letting old references drain naturally, implement the separately
justified stable-parent FUSE FD and strict state-handoff protocol. Do not weaken
writable-handle safety to approximate that feature.
