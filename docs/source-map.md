# Source Map

This map defines the local sources agents should inspect before making source-specific `r9p` claims.

## r9p Source

- `crates/r9p/src/codec.rs`
  - 9P frame encoding/decoding.
  - Message-size math, read/write payload limits, stat-entry chunking.
- `crates/r9p/src/message.rs`
  - T-message and R-message shape.
  - Tags, `NOTAG`, and protocol variants.
- `crates/r9p/src/fid.rs`
  - Fid state and `NOFID`.
- `crates/r9p/src/flush.rs`
  - Live-tag table, duplicate-tag rejection, flush and stale-completion behavior.
- `crates/r9p/src/server/`
  - Generic file-tree trait, session state, open/read/write/stat/walk handling.
- `crates/r9p/src/client.rs`
  - Runtime-neutral client operation builder and response admission.
- `crates/r9p/src/multiplex/`
  - Layered blocking transport facade for concurrent tagged client calls.
- `crates/r9p/src/stat.rs`
  - 9P stat record shape and mode helpers.
- `crates/r9p/tests/memory_tree.rs`
  - Minimal end-to-end server/client fixture.
- `crates/r9p-cli/src/`
  - The `r9p` binary and one-shot client command dispatch.
- `crates/r9p-cli/tests/cli_machine.rs`
  - Machine-output and streaming command regression tests.

Use these when the question is "what does `r9p` do now?"

## Plan 9 And plan9port

- `refs/plan9port/src/cmd/9p.c`
  - plan9port one-shot 9P client command behavior.
- `refs/plan9port/man/man1/9p.1`
  - documented plan9port `9p` command behavior.
- `refs/plan9port/include/9pclient.h`
  - plan9port client library API.
- `refs/plan9port/src/lib9p/`
  - plan9port server library behavior.
- `refs/plan9port/man/man9/`
  - 9P message reference pages.
- `refs/plan9port/src/cmd/acme/xfid.c`
  - Acme 9P file behavior when an Acme-specific compatibility question appears.

Use plan9port when the question is "what does the established 9P ecosystem expect?"

## Racme

- `refs/racme/docs/decisions.md`
  - Decision 6: `r9p` as substrate primitive and extraction trigger.
- `refs/racme/docs/arch/9p-as-substrate.md`
  - Boundary between `r9p`, backends, consumers, transports, and OS bridges.
- `refs/racme/docs/plan/03-m2-headless-9p.md`
  - M2 protocol commitments and Acme adapter boundary.
- `refs/racme/crates/racme-acme/`
  - Acme backend consuming `r9p`.

Use Racme when changing extraction-boundary claims or Acme-backed server behavior.

## r9pfuse

- `refs/r9pfuse/crates/r9pfuse/src/p9.rs`
  - Blocking TCP client facade currently layered over `r9p` primitives.
- `refs/r9pfuse/crates/r9pfuse/src/fuse.rs`
  - FUSE/POSIX-to-9P translation.
- `refs/r9pfuse/crates/r9pfuse/src/node.rs`
  - Nodeid, fid, and directory-entry bookkeeping.
- `refs/r9pfuse/docs/source-map.md`
  - Source map for FUSE bridge behavior.

Use `r9pfuse` when deciding whether a client primitive should move into `r9p` or remain FUSE-specific.

## Vault

- `refs/vault/docs/operations/9p-endpoint.md`
  - Vault 9P listener policy and backend contract.
- `refs/vault/docs/operations/plan9port-client.md`
  - Current operator workflows for `9p`, `9pfuse`, and kernel `v9fs`.
- `refs/vault/docs/source-map.md`
  - Vault source-grounding map.

Use Vault when validating namespace endpoint expectations, attach policy, or mount-client behavior.

## FUSE References

- `refs/r9pfuse/refs/linux-fuse/include/uapi/linux/fuse.h`
  - Linux FUSE protocol ABI.
- `refs/r9pfuse/refs/linux-fuse/fs/fuse/`
  - Linux kernel FUSE implementation.
- `refs/r9pfuse/refs/libfuse/include/`
  - libfuse userspace API headers.
- `refs/r9pfuse/refs/libfuse/example/`
  - Mature FUSE filesystem examples.
- `refs/9pfuse/`
  - Current C `9pfuse` bridge behavior.

Use FUSE sources only for bridge behavior. They do not define 9P semantics.

## Source Reading Notes

Write source-reading notes under `notes/source-reading/`.

Each note should include:

- Date.
- Question being answered.
- Files/functions inspected.
- Source-backed findings.
- Effect on `r9p` docs, plans, or code.
- Open questions.
