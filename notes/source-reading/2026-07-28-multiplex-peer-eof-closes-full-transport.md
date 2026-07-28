# Multiplex peer EOF closes the full transport

Date: 2026-07-28.

## Question

Why did an r9wm terminal viewer retain a `CLOSE-WAIT` connection after the
replaceable compute front restarted?

## Sources inspected

- `crates/core/src/multiplex/reader.rs`
- `crates/core/src/multiplex/client.rs`
- `crates/core/src/multiplex/mod.rs`
- `crates/session/src/client_session.rs`
- `crates/session/src/client/namespace.rs`
- r9wm `crates/terminal/client/src/client.rs`

## Findings

The multiplex client clones one full-duplex transport into reader and writer
handles. Peer EOF made the reader fail all pending requests and exit, but it
did not shut down the shared transport. The still-owned writer handle
therefore left the TCP connection in `CLOSE-WAIT` until its entire logical
client was dropped or another operation explicitly shut it down.

A terminal viewer can legitimately keep an idle namespace client for a long
time after a referred service generation changes. Resource cleanup therefore
cannot depend on a later domain operation noticing the stale route. Peer EOF
is already definitive transport evidence, so the generic multiplex reader
must close the full transport as part of its terminal transition.

## Effect

Every terminal exit from the multiplex reader now shuts down its transport
after publishing the terminal error to pending callers. This wakes the peer,
closes the cloned writer half, and applies equally to TCP, Unix, and
authenticated streams. A transport-level regression half-closes the server
side and proves that the client responds by closing its own remaining half.

No terminal-specific reconnect or cleanup path is added.
