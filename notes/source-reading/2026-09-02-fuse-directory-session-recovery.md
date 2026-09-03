# FUSE Directory Session Recovery

## Question

Why could a long-running r9p FUSE mount return `EIO` after its registered
service restarted even though the mount process, Coordinator attachment, and
change feed remained healthy?

## Sources inspected

- `crates/fuse/src/fuse/ops/dir.rs`
- `crates/fuse/src/fuse/ops/io/read.rs`
- `crates/fuse/src/fuse/dispatch.rs`
- `crates/fuse/src/node/handles.rs`
- `crates/session/src/client/namespace.rs`
- `crates/session/src/client_session.rs`
- `refs/linux-kernel/fs/fuse/readdir.c`

## Findings

The data path and change feed are independent sessions. A connected change
feed therefore does not prove that a FUSE directory handle still owns a live
data fid.

Read-only file handles already reopen their path and replace the handle binding
after a definitive transport failure. Directory handles retained an opened 9P
fid and incremental byte offset, but `READDIR` and `READDIRPLUS` propagated the
first failed read directly to Linux. A file browser could keep that poisoned
handle indefinitely while fresh direct namespace operations remained healthy.

Linux FUSE identifies an open directory stream by its file handle and supplies
the logical continuation offset on every directory read. The bridge can renew
that read-only stream without replaying a mutation: reconnect the namespace
attachment, walk and reopen the directory path, replace only the handle's data
binding, retain its observed entries and remote byte offset, then retry the
failed read once. Directory mutation during enumeration remains subject to the
ordinary POSIX readdir consistency limits.

A root-session replacement also has to mark every cached node binding stale
before installing a fresh attachment. Source resolution waits within the
existing lookup deadline for a transient service-publication gap. If that
deadline expires, the next request must rewalk its path instead of combining a
new client with an old fid. Authentication, protocol, and policy failures remain
immediate terminal errors.

## Decision

FUSE owns read-only directory-handle renewal at the same boundary as read-only
file-handle renewal. Mutation operations remain fail-closed and are never
replayed after an unknown delivery result. Mount status reports data-session
health separately from change-feed health so a live feed cannot mask a failed
filesystem data path.

The host-gated regression keeps a directory large enough to require multiple
kernel reads open across an export restart, then proves enumeration completes
through the same FUSE mount and records one transport renewal.
