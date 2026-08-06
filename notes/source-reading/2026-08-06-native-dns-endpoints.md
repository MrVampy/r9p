# Native DNS Endpoints

## Question

Where should a reverse-connected 9P service preserve and resolve a native host
name instead of converting it to one IP during configuration parsing?

## Sources inspected

- `crates/core/src/blocking.rs`, especially
  `connect_tcp_stream_with_timeouts`.
- `crates/core/src/export_descriptor.rs`, especially `SessionEndpoint` and
  `validate_transport_auth`.
- `crates/session/src/connection_config.rs` and
  `crates/session/src/client_session.rs`.
- `crates/reverse/src/export.rs`, especially `ReverseExportConfig` and
  `export_loop`.
- `crates/cli/src/commands/reverse.rs`, especially `parse_export_config`.

## Findings

The ordinary r9p client already retains its configured address as text and
resolves it whenever a connection is established. Reverse export instead
resolved `--connect` once during CLI parsing and stored one `SocketAddr` for
every later reconnect. That made a DNS name cosmetic and prevented a reconnect
from observing an updated address.

A TCP listener still needs a concrete `SocketAddr`. A dial target or advertised
referral does not. The reusable split is therefore a syntactically validated
`TcpEndpoint` that preserves host and port, followed by name resolution inside
each connection attempt. DNS remains host mechanism; r9p retains only the
generic endpoint and reconnect behavior.

## Effect

`TcpEndpoint` is a runtime-neutral r9p value used by descriptors and reverse
connect. Reverse export resolves it for every connection attempt and tries all
returned addresses within the configured per-address timeout. Bind endpoints
remain concrete socket addresses.
