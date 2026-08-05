# Layered Reverse Session Authentication

Date: 2026-07-27.

## Question

Can a reverse-connected application service authenticate its final 9P client
without trusting the broker's loopback proxy as caller identity?

## Sources Inspected

- `crates/reverse/src/broker.rs`
- `crates/reverse/src/export.rs`
- `crates/reverse/src/tests.rs`
- `crates/auth/src/handshake.rs`
- `crates/auth/src/stream.rs`
- `crates/auth/src/p9any.rs`
- `crates/core/src/server/connection.rs`
- `crates/core/src/multiplex/mod.rs`
- `README.md`
- `docs/design/architecture.md`

## Findings

The reverse broker authenticates the exporter and protects the outbound
placement stream. Its loopback proxy intentionally performs byte bridging only.
An application that accepts an attach username directly on that stream would
therefore trust a caller-controlled identity from any process able to reach the
proxy.

The existing p9any and Noise handshake is already generic over `Read + Write`
except for its TCP timeout and socket-configuration seam. `SecureStream`
already supplies cloneable 9P server and multiplexed-client transport behavior.
Making the underlying transport generic and delegating handshake timeout
control to the ultimate TCP stream allows the same authentication protocol to
be layered through an established reverse stream.

The resulting boundaries remain distinct:

- exporter-to-broker authentication proves placement and protects the reverse
  link;
- client-to-service authentication proves the final service principal; and
- the broker continues to parse neither 9P nor application admission policy.

## Effect

`r9p-auth` now permits p9any/Noise authentication over an existing
`SecureStream`. `ReverseExport` exposes authenticated tree and handler
constructors that bind the verified peer principal into the 9P server session
before version negotiation.

The reverse integration test proves that an unadmitted key cannot use the
loopback proxy while the exact admitted service principal can read the
application-owned tree.

## Open Questions

None for the transport boundary. Registration and namespace selection remain
application and coordinator policy rather than r9p behavior.
