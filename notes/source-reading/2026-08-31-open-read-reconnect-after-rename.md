# Open Read Reconnect After Rename

Date: 2026-08-31

## Question

How should an r9p FUSE read-only file handle recover when a service generation
ends after the file has been renamed but the retained file identity and bytes
are unchanged?

## Sources Inspected

- `refs/9front/sys/src/lib9p/srv.c`, `swstat`, which rejects attempts to change
  `qid.path` or `qid.vers` through wstat.
- `refs/plan9port/src/lib9p/file.c`, file creation and walk behavior, where the
  qid belongs to the file node rather than its directory name.
- `refs/libfuse/example/passthrough_fh.c`, `xmp_read`, which ignores the path
  and reads through the handle retained by `open`.
- `crates/fuse/src/fuse/ops/io/read.rs`, replay of read-only handles after a
  transport or namespace-shape failure.
- `crates/fuse/src/fuse/mount_state.rs`, path-backed node rebinding.
- `crates/fuse/src/node.rs`, qid-to-inode identity and retained node paths.
- `crates/session/src/cache.rs`, bounded decoding of one opened directory.

## Findings

An ordinary rename does not replace a file. Its `qid.path` remains the server's
file identity, and a FUSE read continues through the open file handle without
consulting the old pathname.

A 9P transport reconnection is different from a rename inside one live
session. Standard 9P does not resume the server-side fid table, so r9p must
reconstruct a safe read-only handle. Path replay is sufficient while the name
is unchanged, but it cannot recover a renamed file.

The bounded protocol-native recovery is to read only the previous parent
directory and accept exactly one entry with the same `qid.path` and qid type.
`qid.version` is modification state and is not part of file identity. No match
means the file is no longer reachable from that parent. Multiple matches are a
server identity violation. Both states must fail closed.

## Effect

Read-only handles may relocate within their existing parent after path replay
returns absence. The client then opens the matched qid and preserves the FUSE
handle presented to the application. Mutating handles remain non-replayable.

Services that publish retained objects must derive `qid.path` from durable
object identity rather than presentation paths. A display-name change must not
create a new qid for the same retained object.
