# Referral session restart recovery

Date: 2026-07-28.

## Question

Why did an M7 namespace client keep returning `ENOTCONN` while resolving the
same admitted operating-service path after that service restarted?

## Sources inspected

- `crates/session/src/client/namespace.rs`
- `crates/session/src/client/paths.rs`
- `crates/session/src/client/direct.rs`
- `crates/session/src/client/tests.rs`
- `crates/core/src/multiplex/client.rs`
- `crates/session/src/error.rs`
- `notes/source-reading/2026-07-25-caller-local-namespace-composition.md`
- `notes/source-reading/2026-07-28-terminal-multiplex-transport-state.md`

## Live finding

The laptop operating service restarted during a normal declarative deployment.
M7's compute service kept its existing transparent direct session for
`/agents/operating/tuxedo`. A later exact-spec `stat` failed after its path had
already been walked, reporting that the 9P transport closed before the
response. The following request also failed.

The namespace client already refreshed referrals after a retryable walk error.
However, refreshing a referral whose mount, endpoint, authority, and generation
were unchanged retained the cached `DirectClient`. That client was already in
terminal transport state, so the retry selected the same dead session.

The path helpers had a second gap. `stat_path_timeout` and
`read_path_timeout` could recover a failure during their walk, but a definitive
transport failure during the following stat, open, read, or clunk escaped
without invalidating the route.

## Correction

Each direct client now carries a private connection identity. On a definitive
direct-session transport failure, the namespace client removes only the
matching failed connection from its route cache. It then refreshes the finite
referrals and establishes a fresh direct session, even when the new referral
has the same public identity.

The bounded read-only path helpers retry their whole operation once when a
post-walk failure proves that the selected direct transport is gone. They do
not retry writes, RPCs, creates, removes, or retained-fid mutations whose
delivery could be ambiguous.

The regression fixture closes the first service connection during `stat`,
accepts a replacement session at the same endpoint under the same referral,
and requires the original `stat_path_timeout` call to finish on that
replacement.

## Ownership

This is generic transparent `9P2000.R` session lifecycle machinery. The
coordinator continues to publish finite admitted referrals, and consuming
services continue to use ordinary namespace paths. Neither Agents nor an
application-specific service owns connection invalidation or reconnection.
