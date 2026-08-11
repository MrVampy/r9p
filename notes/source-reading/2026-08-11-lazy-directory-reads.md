---
title: Lazy directory reads across FUSE and Front
date: 2026-08-11
sources:
  - crates/fuse/src/fuse/ops/io/open.rs
  - crates/fuse/src/fuse/ops/dir.rs
  - crates/session/src/cache.rs
  - crates/front/src/tree.rs
  - crates/front/src/model.rs
  - crates/core/src/server/handlers/read.rs
  - refs/coordinator/refs/linux-fuse/fs/fuse/readdir.c
  - refs/coordinator/refs/libfuse/include/fuse_lowlevel.h
---

# Lazy directory reads across FUSE and Front

## Finding

The current FUSE bridge turns every `opendir` into a complete 9P directory
read. `read_open_directory_entries` advances the 9P byte offset until EOF, and
the FUSE handle then serves logical entry cookies from that complete vector.
This makes a demand-backed 9P directory eager before Linux has requested even
its first `readdir` buffer.

Linux FUSE does not require that eager snapshot. The low-level contract gives
`readdir` a byte budget and requires nonzero offsets to be cookies previously
returned by the filesystem. A handle can therefore retain the opened 9P fid,
translate its own logical entry cookies to an incrementally filled vector, and
read another 9P chunk only when the requested FUSE buffer reaches the end of
that vector. The opened 9P fid remains the snapshot boundary.

Front currently supports demand-backed files through read relays, but its
directories are only pushed `Body::Dir` values. A generic directory relay
needs a different completion contract from a file read relay: the publisher
materializes named children, completes a page with those child names, and
Front retains the ordered child snapshot on the requesting fid. Front, rather
than the publisher, continues to encode `Stat` records and enforce 9P directory
offsets.

## Boundary

- FUSE owns incremental Linux-cookie to 9P-byte-offset translation.
- Front owns per-fid ordered directory snapshots and 9P `Stat` encoding.
- A registered application owns when and how another page is obtained and
  which children it materializes.
- Directory reads remain independent per open fid; unrelated directory handles
  can progress concurrently.
- A publisher response names only children it has already placed below the
  relayed directory. It cannot inject raw encoded directory bytes.

This keeps demand policy out of the protocol core and avoids a source-specific
pagination API in either FUSE or Front.
