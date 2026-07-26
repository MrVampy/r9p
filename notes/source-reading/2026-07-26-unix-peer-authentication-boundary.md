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

- Noise session authentication already creates a connection-bound
  `PeerIdentity` inside r9p after verifying both the static key and claimed 9P
  principal.
- Unix sockets expose kernel-authenticated peer credentials through
  `SO_PEERCRED`. Converting that host identity into allowed 9P principals is
  transport authentication, not namespace governance.
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

- Add `PeerCredentialConfig` and `TransportIdentity` to `r9p-auth`.
- Read `SO_PEERCRED` in r9p, map the UID to an explicit set of allowed
  principals, and reject unconfigured peers.
- Match every attached `uname` against the connection-bound transport identity.
- Keep host-specific UID mappings in Nix-generated r9p configuration.
- Remove Unix UIDs and dynamic transport-principal bindings from Coordinator
  policy and control messages.
- Keep service grants, attach rules, capability scopes, and namespace
  admission in Coordinator.
- Use r9p's protocol defaults rather than publishing protocol sizing from
  Coordinator governance state.

## Security boundary

Authentication and admission are independent gates:

1. r9p proves which principals a connection may present.
2. Coordinator decides what an authenticated principal may do in the
   namespace.

A valid transport identity creates no namespace authority by itself. A
Coordinator grant creates no valid transport identity by itself.

## Open questions

- Services sharing one Unix UID cannot be isolated from each other by
  `SO_PEERCRED`; stronger service isolation requires distinct UIDs or
  service-specific Noise identities.
- Loopback TCP remains an explicit local-trust transport in generic r9p.
  Deployments that need process-level identity should select Noise or Unix
  peer authentication instead.
