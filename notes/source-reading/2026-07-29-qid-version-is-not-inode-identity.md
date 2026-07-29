# Qid Version Is Not Inode Identity

Date: 2026-07-29

## Question

Why did a long-lived process working in an r9p FUSE workspace retain a current
directory reported as `(deleted)` after an ordinary operating-host tool turn?

## Sources Inspected

- `refs/plan9port/man/man9/0intro.9p`, qid identity and version semantics.
- `refs/plan9port/man/man9/stat.9p`, `qid.vers` and `qid.path` definitions.
- `refs/plan9port/src/lib9p/srv.c`, `rwrite`, which advances `qid.vers` after a
  successful write without changing `qid.path`.
- `crates/fs/src/unix_io.rs`, `stat_from_libc`, which maps host mtime to
  `qid.version` and device plus inode to `qid.path`.
- `crates/fuse/src/node.rs`, `NodeTable::insert_node`,
  `same_inode_identity`, and `qid_to_inode`.
- `refs/coordinator/refs/linux-fuse/fs/fuse/fuse_i.h`,
  `fuse_stale_inode`, which treats the FUSE generation as inode-lifetime
  identity.
- Agents `crates/runner/src/operating_service/tree.rs`,
  `OperatingTree::map_workspace_qid`, which remaps local workspace qids into a
  collision-free service qid space.

## Findings

In 9P, `qid.path` identifies the file. `qid.version` changes when that same
file is modified. A version change must therefore refresh metadata and cached
contents without creating a new inode identity.

The operating workspace remapper used the complete local `Qid`, including
`version`, as its identity key. A directory mtime change consequently allocated
a new virtual `qid.path`. The FUSE node table separately treated any complete
qid change as a new FUSE generation.

A live three-host probe made the failure deterministic. A sleeping M7 process
started with the projected `trading` directory as its cwd. Before a directory
entry change, both the cwd and path resolved to inode
`4611686018427387968`. After adding one entry and allowing the one-second
lookup cache to refresh, the same namespace path resolved to inode
`4611686018427387969`, while `/proc/<pid>/cwd` reported `(deleted)` and stat
returned EIO. The local directory itself retained its host inode; only its
mtime and therefore qid version changed.

## Effect

Agents must key workspace qid remapping by mount identity, local qid type, and
local `qid.path`, then project the latest `qid.version` onto the stable virtual
path.

r9p FUSE must keep a node's FUSE generation stable when only `qid.version`
changes. A true `qid.path` or file-kind replacement still advances generation.
Directory cache invalidation on version change remains necessary and is
separate from inode identity.

## Open Questions

None for this defect. The live regression must still be rerun through the exact
Kitty, r9wm, namespace, M7 Codex, and laptop operating-host composition after
both corrections are deployed.
