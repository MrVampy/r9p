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
- `crates/auth/src/p9any.rs`: `negotiate_client` and `negotiate_server`.
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

P9any is server-first: `negotiate_server` writes the protocol offer before the
client sends its selection. Waiting for final client bytes before starting the
server therefore deadlocks. Starting the server while the stream is merely
pooled instead places an expiring protocol offer in an unclaimed stream.

The broker is the component that knows when it pairs a pooled stream with a
local consumer. It should send an authenticated, fixed session-claim marker
over the placement stream before it begins the blind byte bridge. The exporter
consumes that marker and then starts either ordinary 9P serving or final
end-service authentication. The final authentication timeout consequently
begins at pairing without exposing service data to the broker.

## Effect on r9p

The generic reverse layer gains a versioned session-claim marker. Every broker
bridge sends it before forwarding application bytes, and every exporter
consumes it before entering the application server. This is one forward-only
reverse transport contract for plain and end-service-authenticated exports.

The end-service authentication regression now keeps the pool idle longer than
the final authentication timeout before connecting a client. This
distinguishes healthy idle capacity from a started but stalled authentication
attempt.

## Open Questions

None for this fix. Reconnection and service re-registration above this generic
transport mechanism remain separate lifecycle concerns.
