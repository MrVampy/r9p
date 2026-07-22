# FUSE Shared Projection

Date: 2026-07-22.

## Question

How can a privileged projection owner expose one bounded r9p mount to a
different admitted local account without changing the 9P authority model?

## Sources inspected

- `crates/fuse/src/fuse/mount.rs`, especially `mount_fuse_attempt` and
  `child_exec_fusermount`
- `crates/fuse/src/fuse/mount_state.rs`, especially `R9pFuse::attr`
- libfuse `util/fusermount.c`, especially `check_allow_permission` and
  `process_generic_option`
- libfuse `util/fuse.conf` and `README.md`
- Linux FUSE `fs/fuse/dir.c`, especially `fuse_allow_current_process`

## Findings

- Linux normally limits a FUSE mount to the mounting account. The explicit
  `allow_other` mount option permits other local accounts to enter it.
- A non-root mounting process needs `user_allow_other` in `/etc/fuse.conf`;
  root may request the option directly.
- `allow_other` is not an authorization policy. The kernel source treats it as
  permission for other processes to reach the FUSE daemon, and libfuse warns
  against relying on cached per-inode permission results across users without
  `default_permissions`.
- r9p attributes use the mounting process identity while the remote 9P server
  remains responsible for filesystem operations. A consumer that uses
  `allow_other` must therefore bound local traversal at the mount's parent and
  treat the admitted 9P export as the authority, not local ownership fields.
- The existing `auto_unmount` retry must retain `allow_other`; retrying without
  it would silently produce a mount with a narrower and misleading access
  contract.

## Effect

The FUSE configuration and `r9p mount` command gain a default-off
`allow_other` option. The fusermount adapter combines it with `auto_unmount`
and preserves it when falling back from helpers that do not support automatic
unmounting. Consumers remain responsible for a restricted parent directory
and explicit admission of every local account that can traverse it.

## Open questions

- None for the bounded shared-projection use case.
