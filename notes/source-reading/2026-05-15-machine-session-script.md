# Machine Session Script Source Reading

Date: 2026-05-15.

## Question

How should `r9p --machine` expose a multi-operation client surface that preserves
one 9P connection and attach while staying backend-neutral?

## Files Inspected

- `src/bin/r9p/main.rs`
- `src/bin/r9p/io.rs`
- `src/bin/r9p/commands/read_write.rs`
- `src/bin/r9p/commands/machine.rs`
- `src/blocking.rs`
- `tests/cli_machine.rs`
- `README.md`
- `docs/architecture.md`

## Findings

The existing one-shot machine commands connect and attach per operation. That is
the right operator shape for independent reads and writes, but it cannot express
session-private state that is established by one write and consumed by a later
read in the same 9P session.

The reusable `blocking::Client` already preserves exactly the needed state:
version negotiation, attach, root fid, path walks, opens, reads, writes, and
clunks all stay on one client value. No protocol-core change is needed for this
slice.

The machine surface should stay generic. It should not know about Vault reloads,
peer mounts, or any backend path policy. A tab-separated script of read/write
operations over one attached client is useful without Vault, and wrappers can
interpret the indexed records above it.

## Effect

`r9p --machine script` is added as a CLI-layer facade over one
`blocking::Client`. It supports bounded hex reads plus streaming local-file
reads and writes, while output remains tab-record based and local-file payloads
avoid argv/stdout payload limits.

The operation set also includes `fresh-stat-error`. That operation opens a
second fresh attach to the same target while the original script session stays
open, and it succeeds only when statting the named path fails in the fresh
session. This is still generic 9P client behavior: it lets wrappers prove
session-local namespace state without teaching `r9p` any backend policy.

## Open Questions

- Whether a later resident client service should use the same operation grammar
  as its request payload, or whether that belongs in a separate typed protocol
  once a second consumer exists.
