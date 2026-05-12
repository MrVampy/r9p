# r9p

`r9p` is the reusable Rust 9P library for substrate-shaped systems. It owns
9P2000 wire types, encoding/decoding, fid/tag/session mechanics, and generic
client/server protocol state. It does not own any particular filesystem,
editor, Vault, FUSE, socket, async runtime, or transport policy.

Current consumers:

- Racme serves an Acme-compatible 9P namespace through `r9p`.
- `r9pfuse` uses `r9p` to mount 9P namespaces through Linux FUSE.

The architectural boundary is deliberately small:

- `r9p` speaks 9P.
- Backends decide what paths mean.
- Clients decide what they consume.
- Runtime adapters own sockets, threads, async tasks, BEAM ports, TLS, and
  FUSE.

## Scope

`r9p` incorporates both sides of the protocol:

- The server core owns session state, fid binding, request admission, and a
  backend-neutral `FileTree` trait.
- The client core owns operation construction, tag/fid allocation, and response
  admission.

The crate does not yet provide a full transport loop or a finished operator
binary. Those should be layered over the reusable client/server core.

## plan9port Compatibility

The operator-facing client should track the shape of plan9port's `9p` command:
`read`, `write`, `stat`, `rdwr`, `ls`, and the address/aname options used by
existing shell workflows. That command is a client facade over the library, not
the boundary of the library itself.

## Development

```bash
cargo test
cargo clippy -- -D warnings
nix flake check
```

See [`AGENTS.md`](AGENTS.md) and [`docs/source-map.md`](docs/source-map.md)
before making protocol, compatibility, or architecture changes.
