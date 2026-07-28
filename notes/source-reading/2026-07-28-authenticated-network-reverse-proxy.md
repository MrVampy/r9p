# Authenticated Network Reverse Proxy Exposure

Date: 2026-07-28.

## Question

How can a remote namespace client follow a `9P2000.R` referral to a service
that reaches the broker through reverse attachment without adding a
host-specific forwarding service or making coordinator carry the byte path?

## Sources Inspected

- `crates/reverse/src/broker.rs`
- `crates/reverse/src/export.rs`
- `crates/reverse/src/session_proxy.rs`
- `crates/cli/src/commands/reverse.rs`
- `docs/architecture.md`
- `docs/source-map.md`

## Findings

`ReverseBroker` authenticated the exporter placement connection and paired
each accepted proxy connection with one bounded reverse stream. Its proxy
listener was restricted to loopback or a local Unix socket.

`ReverseExport::start_authenticated` and
`ReverseExport::start_authenticated_handler` establish a second p9any/Noise
session after the broker assigns the placement stream. That second session
authenticates the final service client at the service boundary. The broker
continues to copy opaque bytes and learns no application policy.

A loopback referral is invalid for a caller on another host. A generic
host-level TCP forwarder would duplicate transport mechanism outside r9p and
would obscure the end-service authentication requirement.

## Effect

The broker now has an explicit `ProxyExposure` contract. `Local` remains the
default. `AuthenticatedNetwork` accepts only a concrete non-loopback,
non-multicast TCP endpoint with a nonzero port. It is reserved for deployments
whose reverse exporter authenticates every final service session.

The CLI exposes the same choice as
`--proxy-exposure authenticated-network`. Namespace admission, service
addressing, firewall policy, and publication remain outside r9p.

## Open Questions

None for the current role-service composition.
