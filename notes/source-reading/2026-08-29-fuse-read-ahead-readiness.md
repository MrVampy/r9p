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
mount owner as `user_id` in the mountinfo super options.

## Source

- `refs/linux-kernel/fs/fuse/inode.c` in the Coordinator workspace,
  `process_init_reply`: derives `ra_pages` from the FUSE reply, updates
  `s_bdi->ra_pages`, sets `conn_init`, and wakes blocked requests.
- `crates/fuse/src/fuse/mount.rs`: the mount becomes visible after
  `fusermount3` passes the FUSE descriptor, before protocol initialization has
  necessarily completed.
- `crates/cli/src/commands/mount/supervisor/read_ahead.rs`: the previous
  command waited only for a mountinfo record and then wrote the backing-device
  value immediately.

## Consequence

The read-ahead command now performs a metadata operation through the mounted
FUSE root under the kernel-reported mount-owner fsuid before writing the
backing-device value as its privileged caller. That operation cannot complete
until FUSE initialization is released. The command restores its original
fsuid, re-reads mountinfo, and requires the same backing-device identity and
owner, so a concurrent remount is retried instead of configuring the wrong
mount.
