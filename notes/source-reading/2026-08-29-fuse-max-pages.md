# FUSE Maximum Request Pages

Date: 2026-08-29

## Question

Why does a mount with a 1 MiB 9P message size deliver cold media reads at
roughly playback rate instead of using the full negotiated payload?

## Sources Inspected

- `crates/fuse/src/fuse/dispatch.rs`
- `crates/fuse/src/fuse/wire.rs`
- `crates/fuse/src/fuse/ops/io/read.rs`
- `crates/fuse/src/fuse/tests.rs`
- `refs/coordinator/refs/linux-fuse/include/uapi/linux/fuse.h`
- `refs/coordinator/refs/linux-fuse/fs/fuse/inode.c`
- `refs/coordinator/refs/libfuse/lib/fuse_lowlevel.c`

## Findings

- r9p requests FUSE protocol 7.31, allocates a 1 MiB payload buffer, returns a
  1 MiB `max_write`, and forwards each kernel read size unchanged to 9P.
- Linux starts a FUSE connection with 32 request pages. It changes that limit
  only when the userspace `FUSE_INIT` response returns `FUSE_MAX_PAGES` and a
  nonzero `max_pages` value.
- r9p did not advertise `FUSE_MAX_PAGES` and returned `max_pages = 0`. On a
  4 KiB-page host, Linux therefore split the intended 1 MiB transfer into
  eight 128 KiB FUSE reads before 9P saw them.
- libfuse derives the page count from the configured maximum request bytes and
  the host page size. For a 1 MiB request, that is 256 pages on a 4 KiB-page
  host and 16 pages on a 64 KiB-page host.
- The live Newsgroups mount had no FUSE congestion and used a direct socket to
  NucBox, but a cold 16 MiB read completed at 1.06 MiB/s. The running MPV
  process consumed 1.13 MB/s for media averaging 1.06 MB/s, leaving almost no
  cold-read margin.

## Effect

The FUSE bridge now advertises `FUSE_MAX_PAGES`, derives `max_pages` from its
existing 1 MiB request bound and the actual host page size, and records the
negotiated request facts in mount diagnostics. The 9P protocol and service
limits do not change.

## Open Questions

Remeasure the same live Newsgroups media after deployment. If corrected direct
9P still lacks substantial seek and bitrate headroom, keep the namespace as
the coordination surface and add a Newsgroups-owned range streaming endpoint
as the specialized media data plane.
