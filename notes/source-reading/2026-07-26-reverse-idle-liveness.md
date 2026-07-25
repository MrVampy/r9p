# Reverse idle-stream liveness

Date: 2026-07-26

## Question

Why did a laptop reverse export retain established sockets after a hard M7
outage while the restarted broker reported no admitted waiting streams, and
where should recovery live?

## Sources inspected

- `crates/reverse/src/export.rs`
  - `export_loop`
  - `retry_delay`
  - `sleep_until_retry`
- `crates/reverse/src/broker.rs`
  - `spawn_reverse_acceptor`
  - `receive_live_stream`
  - `bridge`
- `crates/reverse/src/lib.rs`
  - `configure_transport_socket`
- `crates/reverse/src/tests.rs`
  - reverse pool and stale-stream tests
- `crates/auth/src/handshake.rs`
  - authenticated handshake timeouts and socket setup

## Findings

The exporter already reconnects with bounded backoff after connect,
authentication, or serving failure. A graceful broker restart closes the
stream, so that loop works without service-specific help.

A hard peer outage is different. An authenticated reverse stream may be
application-idle while it waits in the broker pool. The transport previously
disabled Nagle but did not enable bounded TCP keepalive. The local kernel could
therefore retain an old stream as established for its long system default,
leaving `serve_connection` blocked and preventing the exporter worker from
returning to `export_loop`.

The broker's two-millisecond `peer_closed` probe only rejects a close already
visible to its own kernel. It cannot make an exporter on another host discover
that the old broker disappeared.

## Effect

Idle-stream failure detection belongs in the generic reverse transport adapter,
not in Agents, Coordinator, or another watchdog. Both accepted and outbound
reverse sockets now use bounded TCP keepalive. Once the kernel reports hard
peer loss, the existing exporter reconnect loop replenishes the new broker.

The 9P protocol and service admission boundaries are unchanged.

## Open questions

The current Linux keepalive policy targets recovery within roughly one minute.
If future non-Linux deployment hosts need the same tighter probe interval and
retry count, extend the platform configuration using `socket2` rather than
adding service-owned heartbeats.
