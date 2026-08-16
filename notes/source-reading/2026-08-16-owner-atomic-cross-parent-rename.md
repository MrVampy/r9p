---
title: Owner-atomic cross-parent rename
date: 2026-08-16
sources:
  - refs/plan9port/man/man9/stat.9p
  - refs/linux-kernel/fs/9p/vfs_inode.c
  - refs/linux-kernel/net/9p/client.c
  - refs/linux-kernel/include/net/9p/9p.h
  - refs/linux-kernel/fs/fuse/dir.c
  - refs/diod/protocol.md
  - refs/diod/src/libnpfs/fcall.c
  - refs/diod/src/libdiod/diod_ops.c
  - crates/fuse/src/fuse/ops/mutate.rs
  - crates/front/src/tree.rs
---

# Owner-atomic cross-parent rename

## Question

How can a Linux rename between two projected directories remain one
application-owner transaction when plain 9P2000 can rename only within one
parent?

## Findings

Plain 9P2000 expresses a name change through `Twstat`. It carries the file fid
and a new final path element, but no destination-directory fid. Linux v9fs
therefore returns `EXDEV` for a cross-parent rename in the plain dialect.
Client-side copy and remove cannot recover the missing atomicity.

9P2000.L adds `Trenameat` message numbers 74 and 75. Its request contains the
old directory fid and name plus the new directory fid and name. The Linux
client sends that one operation, and diod maps it to one owner-side `rename`
call. Linux FUSE similarly sends the ordinary `FUSE_RENAME` request when the
rename flags are zero, including both parent node IDs.

The r9p session client already negotiates 9P2000.R so it can follow namespace
referrals. Extending that declared dialect with the exact `Trenameat` wire
shape provides the missing operation without claiming the rest of 9P2000.L or
adding an application-specific control file.

## Effect on the implementation

Core admits `Trenameat` only after 9P2000.R negotiation, validates both direct
child names and directory fids, reserves both fids for the request, and calls
one `FileTree::rename_at` owner method. Namespace clients reject endpoints on
different referred service sessions with `EXDEV`; they never copy bytes or
remove a source as a substitute.

Front exposes a relay scoped to one registered owner subtree. Both parents
must be below that same root. The request preserves the exact byte names and
separate principal-relative and Front-relative parent contexts. The
application owner commits or rejects the complete move and updates its Front
projection before accepting the relay. Same-parent FUSE rename continues to
use `Twstat`, while cross-parent FUSE rename sends the one owner operation and
updates only its local node bindings after success.
