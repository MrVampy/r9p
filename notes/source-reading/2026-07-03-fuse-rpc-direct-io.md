# FUSE RPC Direct I/O

Date: 2026-07-03

## Question

Should an `r9p mount` file opened read-write use `FOPEN_DIRECT_IO` when the file is a zero-length namespace app or RPC surface that returns data only after a same-fid write?

## Files Inspected

- `crates/fuse/src/fuse/util.rs`
- `crates/fuse/src/fuse/ops/io/open.rs`
- `crates/fuse/src/fuse/ops/io/read.rs`
- `crates/cli/src/commands/read_write.rs`
- `crates/cli/tests/cli_machine.rs`
- `refs/vault/refs/libfuse/include/fuse_lowlevel.h`
- `refs/vault/refs/libfuse/include/fuse_common.h`
- `refs/vault/refs/linux-fuse/include/uapi/linux/fuse.h`
- `refs/vault/refs/linux-fuse/fs/fuse/file.c`

## Findings

- The raw `r9p rpc` command opens the target `ORDWR`, writes the request at offset 0, then reads the response from the same fid.
- The CLI machine tests already model this with per-fid RPC responses, so same-fid coupling is an existing `r9p` client contract.
- Linux FUSE `FOPEN_DIRECT_IO` bypasses the page cache for the open file, and libfuse documents that direct I/O makes the read syscall reflect the filesystem read result.
- The kernel read path chooses `fuse_direct_read_iter` when `FOPEN_DIRECT_IO` is set, and otherwise uses the cached read path.
- `OREAD=0`, `OWRITE=1`, `ORDWR=2`, and `OTRUNC=0x10`, so the FUSE open helper must test the low access-mode bits rather than exact mode equality.

## Effect

`fuse_open_flags` should return `FOPEN_DIRECT_IO` for read-capable regular file opens: `OREAD` and `ORDWR`, including `ORDWR | OTRUNC`. Pure write-only opens stay non-direct. A host-gated FUSE test should prove a zero-length RPC file can be opened read-write, written, seeked to offset 0, and read back through the same mounted file descriptor.

## Open Questions

- None for this slice.
