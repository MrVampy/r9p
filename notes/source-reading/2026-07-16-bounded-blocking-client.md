# Bounded Blocking TCP Client

Date: 2026-07-16.

## Question

Which transport bounds belong in the reusable blocking 9P client when an
application needs a finite registration attempt?

## Sources Inspected

- `crates/core/src/blocking.rs`: existing blocking TCP connection and 9P
  handshake order.
- `crates/cli/src/transport.rs`: existing resolved-address
  `TcpStream::connect_timeout` loop and socket timeout setup.
- `crates/core/src/codec.rs`: version and attach frame reads and writes.
- `crates/core/src/multiplex/client.rs`: the separate concurrent client facade.

## Findings

TCP connection, read, and write deadlines are generic transport concerns. A
bounded blocking connection must resolve the endpoint, try each resolved
address with `TcpStream::connect_timeout`, then install read and write timeouts
before the first 9P version request. Otherwise a reachable peer that accepts a
socket but stalls during version or attach can still block the caller forever.

Service-registration paths, retry cadence, endpoint authority, and the meaning
of the bytes written after attach are application policy. They do not belong in
the reusable client.

## Effect

`crates/core` now exposes `TcpConnectionTimeouts` and
`Client::connect_tcp_with_timeouts`. The existing `Client::connect_tcp` API and
behavior are unchanged. Deterministic loopback regressions hold a connection
open without replying at the version and attach boundaries, proving that the
configured read timeout bounds both stages. A successful-handshake regression
also verifies that independent read and write timeout values reach the socket.

## Open Questions

Name resolution still uses the standard blocking `ToSocketAddrs` interface;
the connect timeout begins once an address has been resolved. Applications
that require bounded DNS resolution need to supply that resolver policy outside
the 9P client.
