# Local Authenticated Session Proxy

Date: 2026-07-27

## Question

How can a capability-bounded local process use a direct authenticated 9P
service session without receiving the host service's transport private key?

## Sources Inspected

- `crates/auth/src/handshake.rs`
- `crates/auth/src/peercred.rs`
- `crates/reverse/src/broker.rs`
- `crates/reverse/src/export.rs`
- `crates/cli/src/commands/reverse.rs`
- `crates/cli/src/commands/con.rs`

## Findings

- Final-service authentication is established before 9P version negotiation.
  The authenticated principal is fixed by the client key and server allowlist.
- `ReverseExport::start_authenticated_handler` binds the resulting principal
  into the server session, so the later 9P attach must claim the same
  principal.
- `ReverseBroker` already proves the useful generic mechanism: a local
  endpoint can bridge opaque 9P bytes while authentication and placement stay
  outside the application protocol.
- A local consumer does not need the private key when a host-owned process
  terminates the authenticated upstream session and exposes only a local,
  bounded endpoint.

## Effect

`r9p session-proxy` owns this generic forward-session mechanism. It binds only
loopback TCP or a local Unix socket, authenticates every upstream connection as
one fixed principal, enforces finite connection and authentication timeouts,
and bounds concurrent sessions. It does not select paths, perform namespace
admission, or interpret 9P.

Agents can use the proxy as host mechanism while retaining selection and
capability policy in its own compute and operating services.

## Open Questions

The current consumer has one sealed operating spec. If a future service admits
several specs to the same compute principal, the consumer must add a narrower
per-composition session boundary before exposing that shared service endpoint
to mutually distrusting local workloads.
