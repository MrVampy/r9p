# Renewable Endpoint Failover

## Question

Where should an r9p client keep multiple equivalent root endpoints so a
long-running namespace projection can survive one listener host becoming
unreachable without adding application-specific reconnect code?

## Sources inspected

- `crates/session/src/connection_config.rs`
- `crates/session/src/client_session.rs`
- `crates/session/src/client/namespace.rs`
- `crates/session/src/transport.rs`
- `crates/fuse/src/fuse/config.rs`
- `crates/fuse/src/fuse/dispatch.rs`
- `crates/cli/src/commands/mount/config.rs`
- `notes/source-reading/2026-08-06-native-dns-endpoints.md`

## Findings

An endpoint set is client transport composition, not 9P wire behavior and not
Coordinator policy. `ConnectionConfig` already represents one authenticated
root attachment, while `ClientSession` owns renewable replacement of that
attachment. The reusable boundary is therefore an ordered `ConnectionSet`
consumed by `ClientSession`.

Every candidate must carry the same username, attach name, message size, and
authentication contract. Only the address may differ. This prevents failover
from changing the logical namespace or weakening responder verification.

Candidate rotation is admitted only for transient connection failures. An
authentication, protocol, or attachment rejection fails closed at the endpoint
that produced it. A replacement after a failed current attachment starts with
the next candidate; an explicit fresh reconnect starts with the current one.
Both paths retain the existing serialized replacement and session-epoch rules,
and neither replays an application operation.

FUSE is the first consumer because a mounted namespace is long-lived and
already rebuilds its source binding after a renewed attachment. The mount CLI
keeps its primary positional endpoint and accepts bounded ordered
`--fallback-endpoint` values. Mount diagnostics and status expose the active
endpoint and complete candidate list so agents and operators can distinguish a
healthy fallback from an unchanged primary.

## Effect

The generic session layer owns endpoint failover. FUSE supplies candidates and
continues to own path and kernel-cache recovery. Applications remain unaware of
Coordinator host names, and no wrapper binary or application-specific retry
loop is introduced.
