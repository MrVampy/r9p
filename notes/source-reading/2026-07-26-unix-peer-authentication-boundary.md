# Unix peer authentication and namespace governance

Date: 2026-07-26

## Question

Where should Unix peer credentials be converted into an authenticated 9P
identity, and where should the resulting identity be admitted to a namespace?

## Sources inspected

- `crates/auth/src/config.rs`
- `crates/auth/src/handshake.rs`
- `crates/auth/src/stream.rs`
- `crates/front/src/serve.rs`
- `crates/front/src/model.rs`
- `crates/core/src/export_descriptor.rs`
- `bindings/gleam/src/r9p.gleam`
- Coordinator `src/native/r9p_listener/src/connection.rs`
- Coordinator `src/native/r9p_listener/src/principal_binding.rs`
- Coordinator `src/native/r9p_listener/src/front_feed_relay.rs`
- Coordinator `src/core/api/r9p/endpoint_policy.gleam`
- Coordinator `src/core/api/r9p/front_feed_publisher.gleam`

## Findings

- Noise session authentication already proves a connection-bound static key
  and carries the 9P principal claimed during the handshake.
- Unix sockets expose kernel-authenticated peer credentials through
  `SO_PEERCRED`. The UID is transport-authenticated evidence, but assigning
  that evidence an allowed 9P principal is namespace governance.
- Coordinator previously stored Unix UIDs in its endpoint Directive and sent
  them to the listener through a mutable front-feed control operation.
- The old listener allowed every principal without an explicit UID binding.
  The binding list therefore acted as a partial deny list rather than a
  complete authentication authority.
- Namespace admission remains a separate decision. A transport-authenticated
  principal can still be denied attachment, capabilities, paths, or operations
  by Coordinator policy.
- The r9p front already owns the safe protocol defaults for maximum message
  size. Its default open `iounit` should likewise be useful without a
  Coordinator control operation.

## Effect on the implementation

- Add `TransportIdentity` to `r9p-auth`.
- Read `SO_PEERCRED` in r9p and expose an opaque subject such as
  `unix-peer:uid:992` without assigning it a namespace principal. Also expose
  the relative `unix-peer:same-user` subject when the peer UID matches the
  listener's effective UID, so an application's own local maintenance identity
  does not require a host-specific numeric UID in policy.
- Expose an authenticated Noise key as an opaque subject such as
  `noise-static-key:<hex>`. A server may keep a small bootstrap allowlist while
  asking r9p to attest otherwise-unlisted keys for higher-level admission.
- Match every attached `uname` against either explicit transport bootstrap
  trust or a subject admission published by Coordinator policy.
- Keep the Coordinator listener key, bootstrap peer trust, and listener
  mechanics in Nix-generated r9p configuration.
- Keep subject-to-principal admission in mutable Coordinator policy. Enrolling
  a service then requires a namespace mutation, not a Coordinator host edit.
- Keep service grants, attach rules, capability scopes, and namespace
  admission in Coordinator.
- Use r9p's protocol defaults rather than publishing protocol sizing from
  Coordinator governance state.

## Security boundary

Authentication and admission are independent gates:

1. r9p attests the transport subject and, only for explicitly bootstrapped
   peers, may preauthorize the claimed principal.
2. Coordinator decides which attested subjects may present which principals
   and what those principals may do in the namespace.

A valid non-bootstrap transport identity creates no namespace authority by
itself. A Coordinator grant creates no valid transport identity by itself.

## Open questions

- Services sharing one Unix UID cannot be isolated from each other by
  `SO_PEERCRED`; stronger service isolation requires distinct UIDs or
  service-specific Noise identities.
- Loopback TCP remains an explicit local-trust transport in generic r9p.
  Deployments that need process-level identity should select Noise or Unix
  peer authentication instead.
