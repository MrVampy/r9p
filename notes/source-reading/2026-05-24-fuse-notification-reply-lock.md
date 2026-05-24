# 2026-05-24 FUSE Notification Reply Lock

## Question

Can Linux FUSE reverse invalidation block ordinary reads through the mount?

## Files Inspected

- `crates/fuse/src/fuse/reply.rs`
- `crates/fuse/src/fuse/change_feed.rs`
- `crates/fuse/src/fuse/ops/io.rs`
- `refs/linux-fuse/fs/fuse/dev.c`
- `refs/linux-fuse/fs/fuse/dir.c`
- `refs/libfuse/lib/fuse_lowlevel.c`

## Findings

- Linux handles `FUSE_NOTIFY_INVAL_ENTRY` by calling `fuse_reverse_inval_entry()`, which walks and invalidates kernel dentry state.
- A live M7 mount wedged with the change-feed thread in `fuse_reverse_inval_entry` while ordinary POSIX readers waited for FUSE replies.
- `r9p mount` used one process-wide reply write lock for both normal request replies and reverse invalidation notifications.
- Holding that lock across a blocking notification lets one background invalidation block unrelated `stat` and read replies even though the backing 9P server is healthy.
- libfuse sends invalidation notifications through the same low-level notification mechanism, but it does not require user code to hold a normal-reply serialization lock while the kernel processes the notification.

## Effect

- `notify_bytes()` no longer takes the normal reply write lock before writing `FUSE_NOTIFY_*` messages.
- Normal FUSE replies keep their serialization guard. Notifications are still one `write_vectored` syscall, but a blocking reverse invalidation can no longer starve ordinary replies behind the reply lock.
- The live `.vault/live` mount was remounted with the fixed binary. Large dry-run reads through both POSIX `dd` and `vault-storage-mechanism mounted-read-utf8` completed without wedging the M7 runtime.

## Open Questions

- A host-gated stress test should keep exercising change-feed invalidation while concurrent readers repeatedly stat and read dynamic files.
