# Machine Streaming Read And Write Commands

Date: 2026-05-15

## Question

How should `r9p --machine` expose large read and write operations without
forcing payloads through argv or whole-response hex strings?

## Files Inspected

- `docs/source-map.md`
- `AGENTS.md`
- `README.md`
- `src/bin/r9p/main.rs`
- `src/bin/r9p/commands/read_write.rs`
- `src/bin/r9p/commands/machine.rs`
- `src/bin/r9p/io.rs`
- `src/blocking.rs`
- `tests/memory_tree.rs`
- `notes/source-reading/2026-05-13-r9p-cli-plan9port-parity.md`
- `refs/plan9port/src/cmd/9p.c`
- `refs/plan9port/man/man1/9p.1`
- `refs/vault/docs/plan/39/migration-inventory.md`
- `refs/vault/src/substrates/auth_listener/api/r9p/client.gleam`

## Findings

Plan9port's `9p read` and `9p readfd` stream remote bytes to stdout in chunks,
and `write` and `writefd` stream stdin to the remote file. Its `readfd` and
`writefd` names are tied to plan9port `openfd`; in `r9p` they remain layered CLI
facades over generic 9P open/read/write.

The existing `r9p --machine read` buffered the whole file and emitted one
hex-encoded tab record. The existing `r9p --machine write` accepted a hex
payload in argv. Those forms are fine for small typed-wrapper operations but are
the wrong transport for large files or generated artifacts.

The reusable `r9p` blocking client already writes in protocol-sized chunks, and
the normal CLI read path already reads in chunks. The missing surface was a
machine-mode command shape that let callers stream bytes without parsing human
output or placing payload bytes in argv.

Vault Plan 39 Lane C needs exactly that generic surface before it can retire the
direct Gleam 9P streaming client. The command remains backend-neutral: no Vault
paths, setup roots, policy, admission, or runtime lifecycle behavior are added
to `r9p`.

During source reading, this checkout's `refs/plan9port` symlink still pointed
through the old `Racme` path and did not resolve. The local sibling
`/home/mrvamp/Dropbox/Projects/plan9port` was used for the source read, and the
workspace symlink was repaired locally to `../../plan9port`.

## Effect

The implementation keeps the existing small-payload machine records:

- `r9p --machine read path` still prints `read<TAB>payload-hex`.
- `r9p --machine write path offset payload-hex` still prints
  `write<TAB>count`.

It adds streaming machine commands:

- `r9p --machine readfd path` streams raw remote bytes to stdout.
- `r9p --machine read-to path local-path` streams raw remote bytes to a local
  file and prints `read<TAB>count`.
- `r9p --machine writefd path` streams stdin with truncating plan9port
  `writefd` semantics and prints `write<TAB>count`.
- `r9p --machine write-at path offset` streams stdin at an explicit remote
  offset and prints `write<TAB>count`.
- `r9p --machine write-from path offset local-path` streams a local file to an
  explicit remote offset and prints `write<TAB>count`.

The CLI integration test starts an in-process TCP 9P server and proves the new
machine commands transfer a 200 KB payload over multiple protocol reads/writes.

## Open Questions

- Whether a future library-level helper should expose the same local-file copy
  loop for non-CLI Rust consumers. No second Rust consumer exists yet, so the
  current change stays in the CLI layer.
