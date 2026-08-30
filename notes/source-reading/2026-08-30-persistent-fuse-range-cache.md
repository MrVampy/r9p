# Persistent FUSE range cache

Date: 2026-08-30

## Question

How can a thin client retain media ranges across r9p mount restarts without
moving service data through Coordinator or weakening namespace coherence?

## Sources inspected

- `crates/fuse/src/fuse/ops/io/read.rs`, `change_feed.rs`, `invalidation.rs`,
  and `status.rs`: current read, stale-node, kernel invalidation, and status
  behavior.
- `crates/session/src/feed/`: the one shared change-feed parser, catch-up,
  backpressure, and coarse-invalidation contract.
- `refs/coordinator/refs/linux-fuse/fs/fuse/file.c` and
  `refs/coordinator/refs/linux-fuse/include/uapi/linux/fuse.h`: Linux page-cache,
  read-ahead, invalidation, and request-size behavior.
- `refs/coordinator/refs/linux-kernel/fs/9p/` and
  `refs/coordinator/refs/linux-kernel/net/9p/`: the comparative Linux v9fs
  cache and revalidation boundary.
- [rclone mount documentation](https://rclone.org/commands/rclone_mount/):
  sparse range population, remote fingerprinting, bounded least-recently-used
  eviction, and the prohibition on overlapping cache owners.
- [JuiceFS cache documentation](https://juicefs.com/docs/cloud/guide/cache/):
  immutable 4 MiB data blocks, local block caching, active metadata
  invalidation, and client-side read-ahead above the kernel page cache.
- [Cloud Storage FUSE file caching](https://cloud.google.com/storage/docs/cloud-storage-fuse/file-caching):
  local repeat-read acceleration, bounded least-recently-used eviction, range
  read policy, and the consistency cost of caching mutable remote data.
- `notes/source-reading/2026-08-12-coherent-materialization-feed-failure.md`,
  `2026-08-28-fuse-subtree-change-feed.md`, and
  `2026-08-29-fuse-max-pages.md`: earlier r9p findings on feed failure,
  subtree invalidation, and the live media read ceiling.

## Findings

- The Linux FUSE page cache helps only while the mount remains alive. It does
  not provide the durable local custody needed for terminal or mount recovery.
- A full eager mirror is the wrong shape for large media trees. Both rclone and
  JuiceFS demonstrate lazy local ranges whose identity is tied to immutable or
  fingerprinted remote content.
- The cache belongs in the FUSE client, below the mounted filesystem interface.
  Coordinator remains the namespace and referral plane, and established 9P
  service bytes continue to use the direct session selected by r9p.
- Kernel invalidation alone is insufficient for a persistent cache. A feed
  event can mark an already-open node stale before its replacement stat is
  known. Persistent cache lookup must stop during that interval.
- A cacheable r9p range therefore requires a read-only handle, a known positive
  length, a nonzero `qid.version`, and a fresh stat. The cache key binds qid
  type, path, version, length, and mtime. A changed generation cannot reach old
  bytes.
- A fixed 4 MiB range is large enough to absorb several ordinary FUSE reads and
  use negotiated 9P payloads efficiently without downloading an entire media
  object after a small seek. Hits read only the requested subrange from local
  disk.
- Cache storage is derivative. Local read or publication failure must be
  observable but must fall through to the authenticated source rather than
  making a healthy mount unusable.
- The cache volume itself must be bound to the logical source and attach
  identity, private to the effective user, single-owner, quota-bounded, and
  atomically populated. This prevents two mount processes or two namespace
  identities from silently sharing mutable cache state.

## Effect

The FUSE product gains an opt-in persistent read-through range cache. Direct
and session-hosted mount configuration carry only a private cache path and a
byte quota. Cache eligibility and coherence remain inside the FUSE adapter;
the 9P protocol core, Front, BEAM port, services, Coordinator, and host
composition do not gain cache semantics.

The mount status reports range size, quota, current bytes, hits, misses,
fetched bytes, evictions, and local read or write errors. The cache survives an
orderly mount replacement and can serve already-populated ranges while their
fresh 9P identity remains proven.

## Open questions

- Measure cold, warm, seek, restart, and source-loss behavior with the same
  Newsgroups media after host composition adopts the cache options.
- Use those measurements before adding speculative parallel prefetch. The 4 MiB
  miss range already provides bounded userspace read-ahead above Linux FUSE.
