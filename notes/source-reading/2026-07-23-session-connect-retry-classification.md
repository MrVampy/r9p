# Session Connect Retry Classification

## Question

Why could an r9p FUSE projection fail immediately with connection refused even
though its configured initial connection timeout was 30 seconds?

## Sources inspected

- `crates/fuse/src/fuse/mod.rs`, especially `R9pFuse::mount`.
- `crates/session/src/client.rs`, especially
  `Client::connect_with_tracker_timeout`, `connect_should_retry`, and
  `connect_error_is_transient`.
- `crates/session/src/transport.rs`, especially `connect_stream`.
- `crates/session/src/error.rs`, especially `client_error`,
  `is_transport_message`, and `transport_errno`.
- `crates/core/src/blocking.rs`, especially `connect_tcp_stream`.
- `crates/reverse/src/broker.rs`, especially `ReverseBroker::start`.

## Findings

`ReverseBroker::start` binds both the reverse listener and the loopback proxy
listener synchronously before it returns. The observed startup failure was not
an unbound-listener race inside the broker.

`R9pFuse::mount` already passes its finite connection timeout to the shared
session client. The session client retries `ECONNREFUSED`, but the local r9p
blocking dial error was rendered as `connect ... (os error 111)` and was not
classified as a transport message. It therefore became `EREMOTEIO`, which is
intentionally not retryable.

This appears during coordinated service restart when Vault can briefly resolve
the prior service descriptor after the old process has closed its loopback
listener and before the replacement process has rebound the same endpoint.

## Effect

The shared session error mapper now recognizes local connect errors carrying an
OS error number as transport failures and preserves that number. Existing
bounded connection retry then handles connection refusal without an
Agents-specific delay, helper, or service. A delayed TCP-listener regression
test proves the complete retry path.

## Open questions

None for the current failure. Connection retry remains finite and continues to
exclude protocol, authentication, and admission failures.
