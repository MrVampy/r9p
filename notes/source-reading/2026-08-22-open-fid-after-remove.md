# Open fids survive namespace removal

## Question

Should removing a Front path invalidate another fid that already opened a file
inside the removed subtree?

## Sources inspected

- `refs/9front/sys/src/lib9p/fid.c`: `closefid` and `removefid`.
- `refs/9front/sys/src/lib9p/srv.c`: `sremove` and `rremove`.
- `refs/diod/src/cmd/diodcli.c`: `cmd_open_remove_read`.
- `crates/front/src/tree.rs`: `FrontTree::remove`, `FrontTree::clunk`, and
  `FrontTree::drop`.
- `crates/front/src/model.rs`: `State::remove_subtree_if_exists`,
  `State::retain_subtree_paths`, and `State::remove_node_recursive`.

## Findings

Plan 9 lib9p removes and closes only the fid carried by `Tremove`. Other fids
retain their own references. Diod has an explicit interoperability regression
requiring a read through an already-open fid to succeed after another fid
removes the file.

Front instead removed every node in the subtree immediately after the owner
accepted the remove relay. Every session's fid binding points into that shared
node map, so unrelated open fids became `ENOENT`. A snapshot read relay could
publish a complete response and still fail its client's explicit EOF read when
the containing directory was removed between those two reads.

## Effect on r9p

Front now counts shared node references held by fids. Namespace removal detaches
the subtree from name lookup immediately, while referenced detached nodes and
their qids remain available until the last fid clunks or its tree closes. The
last release collects the unreachable subtree. This preserves normal removal
visibility without weakening fid lifetime or retaining detached state forever.
An unresolved child walk remains a namespace lookup rather than open-fid I/O,
so detaching its parent cancels that pending resolution with `ENOENT` even when
another fid keeps the parent's node alive.

## Open questions

None for the Front lifecycle. Filesystem-backed servers retain their own inode
and file-descriptor semantics and do not use this in-memory node ownership.
