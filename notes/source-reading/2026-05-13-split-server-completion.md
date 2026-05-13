# Split Server Completion For Vault Port Process

Date: 2026-05-13.

## Question

What generic `r9p` server capability is needed before Vault can build a
non-default Rust port process that keeps 9P protocol state in Rust while
delegating namespace operations to BEAM?

## Files Checked

- `src/server.rs`
- `src/flush.rs`
- `src/message.rs`
- `docs/architecture.md`
- `refs/plan9port/src/lib9p/srv.c`
- `refs/plan9port/include/9p.h`
- `refs/plan9port/man/man9/flush.9p`
- Vault Plan 37: `refs/vault/docs/plan/37/rust-server-port-protocol.md`

## Findings

`r9p::server::Server::handle` was synchronous: a `TMessage` was admitted,
the `FileTree` backend was called immediately, and the request key was
finished before returning an `RMessage`. That is fine for Racme-style and
fixture-style blocking backends, but it does not let a Vault port adapter
admit a request, send a namespace-operation RPC to BEAM, process `Tflush`, and
then drop a stale backend completion.

The reusable state already existed in `src/flush.rs`: `RequestTable`,
`RequestKey`, and `FlushOutcome` encode live tags, duplicate-tag rejection,
generation counters, and stale-completion rejection. The missing piece was a
public server API that exposes backend dispatch and completion without adding
an async runtime or socket ownership.

Plan9port `lib9p` keeps request objects live until `respond`, lets `Tflush`
find the old request by tag, and treats later responses through request-pool
admission. The exact C object lifetime does not need to be copied, but the
shape confirms that request admission and eventual response are distinct from
wire read/write.

## Change

`src/server.rs` now exposes a split server path:

- `ServerEvent` returns an immediate `Reply`, a backend `Dispatch`, or a
  `Flush` with the `FlushOutcome`.
- `ServerRequest` carries the `RequestKey` and a typed
  `ServerRequestKind` describing the backend operation.
- `ServerCompletion` carries a typed backend result.
- `Server::complete` admits the completion only if the original
  `(tag, generation)` key is still live; flushed or reset requests return
  `None` and do not mutate fid state.

The split API is available without requiring the backend type to implement
`FileTree`, so a port adapter can store its own backend handle and still reuse
`r9p` for session/tag/fid mechanics. The existing synchronous `Server::handle`
path remains intact for current consumers.

## Effect

This closes the generic `r9p` blocker named by Vault Plan 37 Group 5: a Rust
port process can now admit protocol requests and complete them after BEAM work
without inventing a Vault-local request table. Vault still needs a non-default
prototype binary and BEAM fixture backend; those belong in Vault, not in
`r9p`.

## Open Questions

- Production Vault parity still needs a noauth/auth decision for `Tauth`.
- Vault fid metadata such as path segments, namespace group id, and directory
  cache state still belongs in the port adapter unless a generic extension
  point proves useful to non-Vault consumers.
