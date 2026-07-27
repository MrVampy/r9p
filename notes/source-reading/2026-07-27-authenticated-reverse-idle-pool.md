# Authenticated Reverse Export Idle Pool

Date: 2026-07-27

## Question

When should final end-service authentication begin for a connection that an
authenticated reverse exporter has already placed in a broker pool?

## Sources Inspected

- `crates/reverse/src/export.rs`: `ReverseExport::start_authenticated`,
  `ReverseExport::start_authenticated_handler`, and `export_loop`.
- `crates/reverse/src/broker.rs`: `receive_live_stream` and `bridge`.
- `crates/auth/src/handshake.rs`: `authenticate_client`,
  `authenticate_server`, and `authenticate_server_inner`.
- `crates/auth/src/stream.rs`: `SecureStream`, `peer_closed`, record framing,
  and transport timeout handling.
- `crates/reverse/src/tests.rs`:
  `reverse_export_authenticates_the_end_service_peer`.

## Findings

The reverse placement handshake and the final end-service handshake are
separate authentication boundaries. The broker terminates placement
authentication, queues the resulting `SecureStream`, and only later pairs it
with a local client in `bridge`.

The authenticated exporter previously called `authenticate_server`
immediately after placement. `authenticate_server_inner` installs the finite
authentication timeout before it waits for the first P9any byte. An idle pooled
connection therefore expired before a local client could be paired with it.
The exporter then reconnected, producing a continuously churning pool.

The final authentication timeout should begin when the paired client first
makes the reverse stream readable. Before that point, the connection is idle
placement capacity rather than a stalled service-authentication attempt. Once
the first byte arrives, the existing bounded timeout remains responsible for
the complete P9any and Noise IK handshake.

## Effect on r9p

`SecureStream` gains a non-consuming bounded readability observation.
Authenticated reverse exports poll that observation while honoring shutdown,
then invoke the unchanged bounded `authenticate_server` handshake. Plain
reverse exports retain their existing immediate serving behavior.

The end-service authentication regression now keeps the pool idle longer than
the final authentication timeout before connecting a client. This distinguishes
healthy idle capacity from a started but stalled authentication attempt.

## Open Questions

None for this fix. Reconnection and service re-registration above this generic
transport mechanism remain separate lifecycle concerns.
