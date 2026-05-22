# Source Map

This map defines the local sources agents should inspect before making source-specific `r9p` claims.

## r9p Source

- `crates/core/src/codec.rs`
  - 9P frame encoding/decoding.
  - Message-size math, read/write payload limits, stat-entry chunking.
- `crates/core/src/message.rs`
  - T-message and R-message shape.
  - Tags, `NOTAG`, and protocol variants.
- `crates/core/src/fid.rs`
  - Fid state and `NOFID`.
- `crates/core/src/flush.rs`
  - Live-tag table, duplicate-tag rejection, flush and stale-completion behavior.
- `crates/core/src/server/`
  - Generic file-tree trait, session state, open/read/write/stat/walk handling.
- `crates/core/src/client.rs`
  - Runtime-neutral client operation builder and response admission.
- `crates/core/src/multiplex/`
  - Layered blocking transport facade for concurrent tagged client calls.
- `crates/core/src/stat.rs`
  - 9P stat record shape and mode helpers.
- `crates/core/tests/memory_tree.rs`
  - Minimal end-to-end server/client fixture.
- `crates/cli/src/`
  - The `r9p` binary and one-shot client command dispatch.
- `crates/cli/tests/cli_machine.rs`
  - Machine-output and streaming command regression tests.
- `crates/fuse/src/`
  - Canonical Linux FUSE bridge over the `r9p` client primitives, exposed as
    `r9p mount`.
- `crates/fs/src/`
  - Read-only local filesystem-backed 9P server adapter used by `r9p serve`
    and `r9p export`.

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

## Historical r9pfuse

- `refs/r9pfuse/crates/r9pfuse/src/p9.rs`
  - Historical blocking TCP client facade that predated the workspace cutover.
- `refs/r9pfuse/crates/r9pfuse/src/fuse.rs`
  - Historical FUSE/POSIX-to-9P translation.
- `refs/r9pfuse/crates/r9pfuse/src/node.rs`
  - Historical nodeid, fid, and directory-entry bookkeeping.
- `refs/r9pfuse/docs/source-map.md`
  - Source map for FUSE bridge behavior.

Use `crates/fuse/src/` for all current mount-client work. Use `refs/r9pfuse`
only as optional bounded historical comparison when the retired source checkout
is present locally and a plan explicitly needs lineage.

## Vault

- `refs/vault/docs/operations/9p-endpoint.md`
  - Vault 9P listener policy and backend contract.
- `refs/vault/docs/operations/plan9port-client.md`
  - Current operator workflows for `r9p`, `r9p mount`, plan9port `9p`, and
    kernel `v9fs`.
- `refs/vault/docs/source-map.md`
  - Vault source-grounding map.

Use Vault when validating namespace endpoint expectations, attach policy, or mount-client behavior.

## FUSE References

- `refs/vault/refs/linux-fuse/include/uapi/linux/fuse.h`
  - Linux FUSE protocol ABI.
- `refs/vault/refs/linux-fuse/fs/fuse/`
  - Linux kernel FUSE implementation.
- `refs/vault/refs/libfuse/include/`
  - libfuse userspace API headers.
- `refs/vault/refs/libfuse/example/`
  - Mature FUSE filesystem examples.
- `refs/vault/refs/9pfuse/`
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
