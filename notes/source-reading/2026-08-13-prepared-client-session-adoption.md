# Prepared Client Session Adoption

## Sources checked

- `crates/session/src/client_session.rs`
- `crates/session/src/client/namespace.rs`
- `crates/session/src/client/tests.rs`

## Finding

`ClientSession::connect` previously established its current logical namespace
client internally. A caller that needed to perform one initial namespace
operation before entering renewable session ownership therefore had to open a
separate authenticated attachment and discard it.

The logical `Client` already owns the root attachment, lazily connected
referral routes, fid table, and shared request tracker needed by the renewable
session. Reusing that exact value is valid; accepting an arbitrary `Client`
and an unrelated reconnect configuration is not.

## Decision

`PreparedClientSession` establishes one client from one concrete
`ConnectionConfig`, exposes it for initial work, and consumes itself to transfer
that same client and configuration into `ClientSession`. The type prevents a
configuration/client mismatch while avoiding an unnecessary authenticated
reconnect. It does not replay the initial operation or change reconnect
semantics.

The same latency-sensitive caller publishes a desired-state measurement file
after attachment. `Client::reconcile_file_at_timeout` therefore composes the
existing bounded walk, open, create, write, and clunk primitives. It advances
only after definitive `ENOENT` or `EEXIST` evidence and never replays an
ambiguous mutation failure.
