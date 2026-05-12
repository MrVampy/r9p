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
  Rust program, FUSE bridge, test harness, future substrate participant
```

The core rule is: `r9p` speaks 9P; backends decide what to serve; consumers decide what to do with the bytes; runtime adapters decide how bytes move.

## Client And Server

`r9p` owns both reusable protocol sides. The server side is the generic session plus backend boundary. The client side is the runtime-neutral operation builder plus response admission boundary. Keeping both sides in one crate is deliberate: tags, fids, stat records, message limits, flush handling, and wire encoding are shared protocol concerns, not application concerns.

The plan9port `9p` command is a client UX target for a future `r9p` binary. It should behave like the established command for shell workflows, while the reusable crate remains broader than that binary and continues to serve embedded clients and servers.

## Non-Goals

- No Racme editor semantics.
- No Vault namespace policy.
- No FUSE/POSIX translation.
- No mandatory async runtime.
- No socket ownership in the protocol core.
- No TLS policy.
- No host-filesystem exporter baked into the library.

## Extraction Rule

`r9p` was seeded inside Racme because Racme needed an Acme-compatible 9P server first. The extraction trigger is a second real consumer. `r9pfuse` is now that consumer, so the project is a standalone repository.

Future work should move reusable 9P client conveniences from `r9pfuse` into `r9p` while keeping FUSE-specific translation in `r9pfuse`.
