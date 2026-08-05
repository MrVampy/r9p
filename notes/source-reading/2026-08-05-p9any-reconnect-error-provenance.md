# P9any reconnect error provenance

Date: 2026-08-05

## Question

Why did a renewable namespace client stop permanently when an authenticated
referral endpoint reset a connection during host boot?

## Sources inspected

- `crates/auth/src/p9any.rs`, especially `read_nul_string` and
  `write_nul_string`;
- `crates/session/src/transport.rs`, especially `connect_stream`;
- `crates/session/src/error.rs`, especially `client_error`,
  `is_transport_message`, and `transport_errno`;
- `crates/session/src/client/direct.rs`, especially the bounded connection
  retry; and
- `crates/session/src/client_session.rs`, especially `reconnect_after`.

## Finding

P9any negotiation converts an operating-system I/O error into the shared r9p
error text while retaining the standard `(os error N)` suffix. The session
adapter recognized socket errors from the initial `connect` call, but not I/O
errors raised while reading or writing the p9any negotiation. It consequently
sent `read p9any protocol offer: Connection reset by peer (os error 104)`
through remote 9P error classification. That classifier matched the word
`protocol` before `connection reset` and returned `EPROTO`, so a renewable
client treated an early-boot transport reset as permanent.

## Decision

The session adapter recognizes p9any read and write failures carrying an
operating-system error number as local transport failures and preserves that
errno. A genuine p9any selection or authentication rejection carries no
operating-system error marker and remains non-transient. This keeps reconnect
policy machine-readable without teaching a domain client about authentication
message text or broadening remote 9P errors into retryable failures.
