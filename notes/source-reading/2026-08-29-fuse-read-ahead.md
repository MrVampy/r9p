# FUSE Read-Ahead After Wider 9P Requests

Date: 2026-08-29

## Question

Why did a live Newsgroups FUSE mount remain limited to roughly 1.3-1.8 MiB/s
after Linux accepted 1 MiB FUSE and 9P reads?

## Sources Inspected

- `refs/coordinator/refs/linux-kernel/fs/fuse/inode.c`
- `refs/coordinator/refs/libfuse/lib/fuse_lowlevel.c`
- `refs/coordinator/refs/libfuse/include/fuse_common.h`
- `crates/fuse/src/fuse/dispatch.rs`
- `crates/fuse/src/fuse/ops/io/read.rs`
- `crates/core/src/multiplex/client.rs`
- [Linux 9P performance patch series](https://patchew.org/linux/20230124023834.106339-1-ericvh@kernel.org/)
- [FTP-like streams for the 9P file protocol](https://repository.rit.edu/theses/3096/)

## Findings

The live mount negotiated `FUSE_MAX_PAGES` with 256 pages and forwarded each
1 MiB FUSE read as one bounded 9P Tread. Its FUSE backing device nevertheless
retained `read_ahead_kb=128`.

Linux supplies that existing backing-device window in `FUSE_INIT`. When the
daemon replies, `process_init_reply` sets the final window to the minimum of
the existing value and the daemon's `max_readahead`. A FUSE daemon therefore
cannot raise the initial 128 KiB value through protocol negotiation.

libfuse follows the same rule by capping its configured value to the kernel's
INIT input. Its documentation classifies read-ahead requests as bounded
background requests. r9p already has the needed 9P tag multiplexing and FUSE
worker capacity, so a larger kernel window can issue several independent
1 MiB reads without changing 9P wire semantics.

Linux v9fs maintainers reported roughly tenfold sequential transfer gains from
the combined larger-MSIZE, readahead, and caching work. The 2010 streaming
research reached HTTP-class throughput through an out-of-band TCP stream,
which remains the next option if the correctly configured cached 9P path is
still insufficient.

## Effect

Add one typed `r9p mount read-ahead` host operation. It accepts only one exact
`fuse.r9p` mount, derives its backing-device coordinate from mountinfo, writes
a bounded read-ahead value, and verifies the observed value. A system service
can invoke it directly after mounting without embedding shell logic or
teaching an application module how Linux names FUSE backing devices.

## Open Question

Measure single-reader and concurrent throughput with a 4 MiB window after the
same-LAN Nebula underlay preference is active. Add an out-of-band artifact
transfer only if that complete over-9P path still lacks useful headroom.
