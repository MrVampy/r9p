# FUSE read-ahead readiness

## Finding

Linux applies the FUSE `max_readahead` reply during `process_init_reply` and
updates the mount backing device's `ra_pages` before setting `conn_init` and
waking blocked requests. A systemd `ExecStartPost` can therefore observe the
mount in `/proc/self/mountinfo`, write `read_ahead_kb`, and still have the
kernel replace that value when FUSE initialization completes.

The live Newsgroups mount demonstrated the exact race. The post-start command
logged 4096 KiB, but the backing device changed to 128 KiB about 300 ms later.

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
FUSE root before writing the backing-device value. That operation cannot
complete until FUSE initialization is released. It then re-reads mountinfo and
requires the same backing-device identity, so a concurrent remount is retried
instead of configuring the wrong mount.
