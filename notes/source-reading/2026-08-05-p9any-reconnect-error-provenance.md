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

P9any negotiation converts a local I/O error into the shared r9p error text.
Most socket errors retain the standard `(os error N)` suffix, but Rust's
`read_exact` renders `UnexpectedEof` as `failed to fill whole buffer` without a
numeric errno. The session adapter recognized socket errors from the initial
`connect` call, but initially recognized p9any read and write failures only
when they retained an operating-system error number.

That first gap sent
`read p9any protocol offer: Connection reset by peer (os error 104)` through
remote 9P error classification. The narrower first repair preserved its
`ECONNRESET`, but a live host-reboot proof then exposed the other rendering:
`read p9any selection response: failed to fill whole buffer`. That message
again reached remote classification, which matched the word `protocol` and
returned `EPROTO`. In both cases a renewable client treated an early-boot
transport break as permanent.

## Decision

The session adapter recognizes every error carrying the local `read p9any` or
`write p9any` provenance as a transport failure. It preserves a numeric
operating-system errno when present; otherwise the broken negotiation maps to
`ECONNRESET`, which is reconnectable. Genuine p9any selection and admission
rejections have distinct messages without those local I/O prefixes and remain
non-transient. This keeps reconnect policy machine-readable without teaching a
domain client about authentication message text or broadening remote 9P errors
into retryable failures.

## Host-absence follow-up

A later real reboot kept the laptop-side terminal viewer alive while the remote
host disappeared from the LAN. Linux returned `EHOSTUNREACH` from the initial
TCP `connect`, and `client_error` correctly preserved that numeric errno. The
transient connection allowlist did not include host or network reachability
errors, however, so a persistent namespace observer stopped instead of waiting
for the namespace change feed and renewing its attachment when the host came
back.

The generic session policy now treats `ENETDOWN`, `ENETUNREACH`, `EHOSTDOWN`,
and `EHOSTUNREACH` as transient connection failures. These values describe the
current reachability of a configured endpoint; they do not weaken admission or
turn a remote 9P rejection into a retry. The exact Linux `No route to host (os
error 113)` rendering is covered by a regression test.
