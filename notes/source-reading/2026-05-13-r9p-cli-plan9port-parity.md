# r9p CLI Plan9port Parity

Date: 2026-05-13

## Question

How should the new `r9p` CLI match plan9port's `9p` command closely enough to replace it for one-shot operator use?

## Files Inspected

- `docs/source-map.md`
- `refs/plan9port/src/cmd/9p.c`
- `refs/plan9port/man/man1/9p.1`
- `refs/plan9port/include/9pclient.h`
- `refs/plan9port/src/lib9/fcallfmt.c`
- `refs/plan9port/src/lib9/dirmodefmt.c`
- `refs/vault/docs/operations/plan9port-client.md`
- `refs/vault/docs/operations/9p-endpoint.md`
- `refs/r9pfuse/crates/r9pfuse/src/p9.rs`
- `refs/r9pfuse/crates/r9pfuse/src/node.rs`
- `src/client.rs`
- `src/codec.rs`
- `src/message.rs`
- `src/server.rs`
- `src/stat.rs`

## Findings

plan9port's `9p` command is a small client facade, not a server or mount bridge. The documented command set is `read`, `readfd`, `write`, `writefd`, `stat`, `rdwr`, and `ls`; the source also exposes `rm`, `create`, and `con`. Global options are `-a address`, `-A aname`, `-n`, and debug `-D`. Without `-a`, `service/subpath` connects through the plan9port namespace socket for `service` and walks `subpath`.

The extracted `r9p` crate already had the protocol pieces needed for a CLI: `src/client.rs` builds operations and admits responses, `src/codec.rs` handles frames, `src/stat.rs` encodes stats and directory streams, and `src/server.rs` remains backend-neutral. The existing dirty change in `src/client.rs` added `Tcreate`, `Tremove`, and `Twstat` operation builders, which the CLI can use for `create` and `rm`.

`r9pfuse` carried a local blocking TCP client facade over `r9p` with address parsing, open/read/write/stat/create/remove helpers, and directory stat-stream decoding. That shape belongs above the sans-I/O core and can be mirrored in `r9p` as a clearly layered blocking facade without moving socket ownership into the protocol core.

Vault docs show the current operator replacement target as plan9port `9p -a 127.0.0.1:9564 ...`, with `scripts/p9` as a Vault-specific helper. The reusable `r9p` CLI should therefore keep plan9port-compatible one-shot commands while remaining generic and backend-neutral.

## Effect

The implementation adds:

- `src/blocking.rs`, a blocking client facade layered over the existing protocol client.
- `src/bin/r9p.rs`, a plan9port-shaped CLI supporting the documented commands plus source-visible `rm`, `create`, and `con`.
- `src/stat.rs::decode_dir_entries`, a reusable decoder for directory read stat streams.

The CLI accepts `-u uname` and `-m msize` as small Rust-client extensions, while preserving plan9port's `-a`, `-A`, `-n`, and `-D` option surface.

## Open Questions

- Whether to install a compatibility alias named `9p` in packaging, or keep the binary only as `r9p`.
- Whether `readfd` and `writefd` should stay aliases over normal 9P open/read/write in Rust, since plan9port's `openfd` path is a plan9port library debugging feature rather than a generic 9P primitive.
