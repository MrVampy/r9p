# `Twstat` And Symlink Contract

Date: 2026-07-21

## Question

Which `Twstat` rules belong in the backend-neutral r9p server, how should a
backend preserve all-or-none mutation, and how should r9p expose its existing
symlink behavior without falsely claiming 9P2000.u?

## Sources Inspected

- `refs/plan9port/src/lib9p/srv.c`, especially `swstat`.
- Vault references `refs/9front/sys/man/5/stat` and
  `refs/9legacy/sys/man/5/stat`.
- Vault references `refs/9front/sys/src/cmd/ramfs.c` and
  `refs/9legacy/sys/src/cmd/ramfs.c`, especially their `wstat` handlers.
- `refs/plan9port/include/libc.h` for `QTSYMLINK` and `DMSYMLINK` lineage.
- r9p `crates/core/src/server/`, `crates/core/src/stat.rs`,
  `crates/fs/src/`, `crates/session/src/`, and `crates/fuse/src/`.

## Findings

Plan9port's server library rejects changes to `type`, `dev`, `qid`, `muid`,
and the directory bit before invoking the server's `wstat` callback. The Plan
9 stat manual also makes `atime` and `uid` immutable and requires explicit
maximum-value or empty-string sentinels for untouched fields.

The Plan 9 ram filesystems validate every requested field and permission before
the first mutation. The stat manual makes the result contractual: success
means every requested change happened, while failure means none happened.
Consequently, a backend that cannot atomically combine two host operations
must reject the combination before either operation.

`QTSYMLINK` and `DMSYMLINK` come from the Unix-oriented 9P2000.u lineage, but
r9p does not implement the rest of 9P2000.u's stat and identity contract. The
honest minimal shape is therefore a named r9p extension, not partial 9P2000.u.
9P's period-suffixed version negotiation already permits a server to accept or
downgrade that request.

## Effect On r9p

- Core validates immutable `Twstat` fields and type bits before backend
  dispatch.
- `Stat::null_wstat()` replaces the duplicated session/FUSE constructor.
- `FileTree::wstat` explicitly requires all-or-none backend behavior.
- The local filesystem backend rejects unsupported fields and combined
  rename-plus-truncate requests before mutation, and rename uses no-replace
  semantics.
- `9P2000.r9p-symlink` is negotiated by the local exporter and FUSE session;
  plain sessions cannot receive symlink qids or stat bits.
- Export descriptors advertise the exact extension instead of plain 9P2000.

## Open Questions

- Full 9P2000.u or 9P2000.L remains unjustified without a named external
  consumer that needs the complete dialect.
- Additional atomic host mutations should be added only with an implementation
  that preserves the all-or-none contract for every supported combination.
