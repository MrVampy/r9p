# Caller-Local Namespace Composition

Date: 2026-07-25

## Question

Where must an r9p client run when coordinator admits a registered service, and
how can direct service sessions remain hidden behind one logical namespace?

## Sources Inspected

- `crates/core/src/referral.rs`
- `crates/core/src/codec.rs`
- `crates/session/src/client/namespace.rs`
- `crates/session/src/client/direct.rs`
- `crates/session/src/authority.rs`
- `crates/fuse/src/fuse/mod.rs`
- `crates/fuse/src/fuse/dispatch.rs`
- `crates/front/src/abi/client.rs`
- `crates/beam-port/src/lib.rs`
- `refs/coordinator/docs/operations/9p-endpoint.md`
- `refs/coordinator/docs/architecture/63-governed-service-addressing.md`

## Findings

- `9P2000.R` adds `Treferrals` and `Rreferrals`; referral records are protocol
  messages and do not appear as namespace files.
- `session::Client` attaches to the admitted root, requests referrals, and
  routes each ordinary walk through the longest matching mounted prefix.
- Direct clients are established lazily in the caller process, share request
  tracking with the root, and remain reusable after the finite referral used
  to establish them expires.
- Expired referrals that have not been connected are refreshed through the
  root before a new service session is attempted.
- Referrals carry portable authority names. `AuthorityBindings` supplies
  caller-local credential configuration without exposing local paths through
  coordinator or embedding service-specific logic there.
- The CLI, FUSE adapter, front ABI, and BEAM adapter all consume the same
  transparent session client. There is no separate resolved-client facade.
- Endpoint reachability and authority materialization are properties of the
  actual caller host. A loopback endpoint cannot represent a remotely
  reachable service to a caller on another host.
- Moving the client onto the service host changes the caller and can conceal a
  broken composition. SSH remains valid for host administration, but it cannot
  stand in for the caller's r9p session.

## Effect

The public resolution namespace and its descriptor facade were removed. One
`9P2000.R` client now composes the admitted root and direct services
transparently, and every caller surface carries the same optional local
authority bindings.

## Open Questions

- coordinator still needs to generate referrals directly from admitted service
  registration state.
- Registered services still need reachable authenticated endpoints or reverse
  attachment for each intended caller host.
