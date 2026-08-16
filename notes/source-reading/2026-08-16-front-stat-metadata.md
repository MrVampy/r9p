---
title: Independent Front modification time and logical length
date: 2026-08-16
sources:
  - refs/plan9port/man/man9/stat.9p
  - crates/front/src/model.rs
  - crates/front/src/front.rs
  - crates/front/src/abi/mod.rs
  - crates/front/include/r9p_front.h
  - crates/fuse/src/fuse/mount_state.rs
---

# Independent Front modification time and logical length

## Question

Can a publisher expose a truthful modification time and logical byte length
through Front without coupling either value to `qid.version` or changing the
FUSE bridge?

## Findings

Plan 9 stat records already carry three independent facts. `qid.version` is the
content version for one `qid.path`, `mtime` is whole seconds since the Unix
epoch, and `length` is the file length in bytes. The protocol does not derive
one from another.

Front previously preserved publisher-owned qid identity and generation but set
`mtime` to the qid version and derived length from its resident body. That made
a truthful upstream posting time impossible and made a synthetic file advertise
the size of its cached representation instead of its logical payload.

The FUSE bridge already translates the received 9P `Stat.mtime` and
`Stat.length` directly into `fuse_attr`. No FUSE-specific metadata path or
invalidation change is needed.

## Effect on the implementation

Pushed Front metadata now requires independent `mtime` and `length` values in
the linked Rust and C ABI contracts. Front serves those exact values while
continuing to use `qid.version` only as content version. Non-pushed Front nodes
retain their existing derived metadata. Pushed directories reject a nonzero
logical length before mutation.

The C ABI advances as one forward contract rather than retaining an older push
signature. A dedicated capability bit lets hosts verify that the loaded Front
understands pushed stat metadata.

## Open questions

None for the 9P2000 surface. Nanosecond modification time would require a
different stat extension and is outside this change.
