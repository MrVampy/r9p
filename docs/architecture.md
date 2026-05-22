# r9p Architecture

`r9p` is the reusable 9P protocol primitive. It is intentionally narrower than a filesystem, narrower than a FUSE bridge, and narrower than any one substrate participant.

## Boundary

```text
backend
  Racme Acme tree, Vault namespace adapter, exportfs-style host tree, memory fixture

r9p server core
  9P messages, qids, fids, tags, stat records, walk/open/read/write/clunk/flush

transport adapter
  Unix socket, TCP, stdio, BEAM port, virtio transport

r9p client core
  9P operation builders and response admission

consumer
  Rust program, FUSE bridge, export helper, test harness, future substrate participant
```

The core rule is: `r9p` speaks 9P; backends decide what to serve; consumers decide what to do with the bytes; runtime adapters decide how bytes move.

## Client And Server

`r9p` owns both reusable protocol sides. The server side is the generic session plus backend boundary. The client side is the runtime-neutral operation builder plus response admission boundary. Keeping both sides in one crate is deliberate: tags, fids, stat records, message limits, flush handling, and wire encoding are shared protocol concerns, not application concerns.

The plan9port `9p` command is the client UX target for the one-shot `r9p`
operations. The installed `r9p` binary now also exposes the generic local
communication suite: `mount` for FUSE import, `serve` for local read-only 9P
serving, and `export` for serving plus descriptor emission. The reusable core
crate remains broader than that binary and continues to serve embedded clients
and servers.

The FUSE mount adapter follows the mature libfuse/Linux concurrency shape: a
bounded worker pool handles kernel requests, and the FUSE INIT reply advertises
bounded `max_background` and congestion settings. This makes recursive walks
and slow peer operations apply backpressure at the mount boundary instead of
spawning unbounded per-request threads in the client process.

## Non-Goals

- No Racme editor semantics.
- No Vault namespace policy.
- No FUSE/POSIX translation in the protocol core. The workspace's `crates/fuse`
  owns that bridge as an adapter above the core.
- No mandatory async runtime.
- No socket ownership in the protocol core.
- No TLS policy.
- No host-filesystem exporter baked into the library.

## Extraction Rule

`r9p` was seeded inside Racme because Racme needed an Acme-compatible 9P
server first. The extraction trigger was a second real consumer: the FUSE
bridge that is now `crates/fuse` and exposed by `r9p mount`.

The repository exception is narrow: `r9p` is one installable communication
suite with internal crates for protocol core, CLI, FUSE bridge, and
filesystem-backed serving. Vault-specific listener glue, editor participants,
plumbers, and domain policy remain outside this repository.
