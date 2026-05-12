# plan9port 9p Client Shape

Date: 2026-05-12

## Question

Should `r9p` work like plan9port's `9p`, and does that imply client-only or both client and server support?

## Files Inspected

- `refs/plan9port/src/cmd/9p.c`
- `refs/plan9port/man/man1/9p.1`
- `src/client.rs`
- `src/server.rs`

## Findings

plan9port's `9p` is an operator-facing client command. It dials or namespace-mounts a 9P server, attaches with an optional aname, and offers shell-friendly commands such as `read`, `write`, `stat`, `rdwr`, and `ls`. The source also contains command handlers for `readfd`, `writefd`, `rm`, `create`, and `con`.

The extracted `r9p` crate already has both protocol sides at the reusable-core layer:

- `src/server.rs` provides `Server`, `Session`, and the backend-neutral `FileTree` trait.
- `src/client.rs` provides `Client`, operation builders, tag/fid allocation, and response admission.

## Effect

The project docs should say that `r9p` incorporates both client and server protocol machinery. A plan9port-compatible `9p`-style tool belongs as a client facade over the library, not as the definition of the library.

## Open Questions

- Whether the future binary should be named `r9p`, `r9p9`, or provide a compatibility alias named `9p`.
- Which source-visible but underdocumented plan9port commands (`rm`, `create`, `con`) should be included in the first parity pass.
