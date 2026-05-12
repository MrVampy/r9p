# r9p Agent Instructions

`r9p` is the reusable Rust 9P protocol crate for the substrate. It is source-grounded: before changing protocol behavior, compatibility claims, architecture docs, or implementation plans, inspect the relevant local sources listed in [`docs/source-map.md`](docs/source-map.md).

## Required Workflow

1. Start from the `r9p` project root.
1. Read [`docs/source-map.md`](docs/source-map.md) for the relevant source paths.
1. Inspect the exact files/functions needed for the change.
1. Record important source findings in `notes/source-reading/` when they affect a plan or design decision.
1. Mention the files/functions checked in the final answer or in the changed plan note.

## Source Priority

- Prefer local source references over web search.
- Prefer Plan 9 / plan9port sources for 9P2000 wire and client behavior.
- Prefer Racme references for the original `r9p` extraction boundary.
- Prefer `r9pfuse` references for real Rust client pressure and FUSE bridge requirements.
- Prefer Vault references for namespace endpoint expectations and operational interop.
- Treat FUSE sources as bridge references; they do not define 9P semantics.

## r9p Invariants

- `r9p` owns 9P wire bytes, message types, qids, fids, tags, version negotiation, flush semantics, stat encoding, dirread chunking, and generic client/server protocol state.
- `r9p` must remain backend-neutral. No Racme, Vault, FUSE, editor, plumber, or host-filesystem semantics belong in the core crate.
- `r9p` must remain runtime-neutral at the core. No socket ownership, tokio requirement, thread policy, BEAM port loop, TLS policy, or FUSE lifecycle belongs in the reusable protocol core.
- Runtime adapters and convenience facades are allowed only when they are clearly layered above the protocol core.
- Acme-specific behavior belongs in Racme's Acme adapter, not in `r9p`.
- FUSE/POSIX translation belongs in `r9pfuse`, not in `r9p`.
- Vault namespace policy, provenance, and admission belong in Vault, not in `r9p`.
- Do not add compatibility or legacy layers for old extraction paths. Update callers to the intended boundary instead.

## Boundary Test

For any proposed feature, ask: would removing Racme, Vault, and `r9pfuse` still leave this useful to another 9P client or server? If yes, it probably belongs in `r9p`. If no, it belongs in the backend, bridge, or runtime adapter that needs it.
