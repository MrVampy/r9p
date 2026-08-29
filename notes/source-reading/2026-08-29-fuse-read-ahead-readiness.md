# FUSE read-ahead readiness

## Finding

Linux applies the FUSE `max_readahead` reply during `process_init_reply` and
updates the mount backing device's `ra_pages` before setting `conn_init` and
waking blocked requests. A systemd `ExecStartPost` can therefore observe the
mount in `/proc/self/mountinfo`, write `read_ahead_kb`, and still have the
kernel replace that value when FUSE initialization completes.

The live Newsgroups mount demonstrated the exact race. The post-start command
logged 4096 KiB, but the backing device changed to 128 KiB about 300 ms later.

The first readiness implementation ran as root because writing
`/sys/class/bdi/.../read_ahead_kb` requires privilege. A normal FUSE mount
without `allow_root` rejects root traversal, so the metadata request failed
with `EACCES` even though the mount was healthy. Linux exposes the admitted
mount owner as `user_id` and `group_id` in the mountinfo super options.

Changing only `fsuid` does not satisfy this FUSE gate. Linux requires the
caller's real, effective, and saved UID and GID to all equal the mount owner.
Libfuse handles the equivalent privileged-helper check by forking, setting the
child's complete UID and GID identity, and reopening the mount as that child.

## Source

- `refs/linux-kernel/fs/fuse/inode.c` in the Coordinator workspace,
  `process_init_reply`: derives `ra_pages` from the FUSE reply, updates
  `s_bdi->ra_pages`, sets `conn_init`, and wakes blocked requests.
- `refs/linux-kernel/fs/fuse/dir.c` in the Coordinator workspace,
  `fuse_permissible_uidgid` and `fuse_allow_current_process`: require matching
  real, effective, and saved UID and GID when `allow_other` is absent.
- `refs/libfuse/util/fusermount.c` in the Coordinator workspace,
  `recheck_ENOTCONN_as_owner`: forks and applies complete owner UID/GID identity
  before checking a user-owned FUSE mount from a privileged helper.
- `crates/fuse/src/fuse/mount.rs`: the mount becomes visible after
  `fusermount3` passes the FUSE descriptor, before protocol initialization has
  necessarily completed.
- `crates/cli/src/commands/mount/supervisor/read_ahead.rs`: the previous
  command waited only for a mountinfo record and then wrote the backing-device
  value immediately.

## Consequence

The read-ahead command now forks one bounded readiness child, applies the
kernel-reported mount owner's complete UID and GID identity in that child, and
performs a metadata operation through the mounted FUSE root. That operation
cannot complete until FUSE initialization is released. The privileged parent
retains authority to write the backing-device value, re-reads mountinfo, and
requires the same backing-device identity and owner, so a concurrent remount is
retried instead of configuring the wrong mount.
