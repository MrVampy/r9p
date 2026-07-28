# Renewable 9P client session

Date: 2026-07-28.

## Question

Which reconnect mechanisms are the same across FUSE, namespace clients,
reverse-connected services, service registration, and r9wm terminal
observation, and which recovery semantics must remain with their consumer?

## Sources inspected

- `crates/session/src/client/namespace.rs`
- `crates/session/src/client/paths.rs`
- `crates/session/src/opened_fid.rs`
- `crates/session/src/slot.rs`
- `crates/fuse/src/fuse/dispatch.rs`
- `crates/fuse/src/fuse/ops/io/read.rs`
- `crates/fuse/src/fuse/ops/io/write.rs`
- `crates/reverse/src/export.rs`
- `notes/source-reading/2026-07-28-referral-session-restart-recovery.md`
- `notes/source-reading/2026-07-22-r9wm-unified-session-client.md`
- coordinator `docs/plan/83/index.md`
- Agents `crates/runner/src/registration.rs`
- Agents `crates/runner/src/profile_service/runtime.rs`
- Agents `crates/runner/src/operating_service/runtime.rs`
- r9wm `crates/terminal/client/src/client.rs`
- r9wm `crates/terminal/attach/src/main.rs`

## Findings

The reusable client-side unit is one renewable attachment to a logical 9P
namespace. It owns immutable connection and authentication configuration, the
current attached client, one session epoch, serialized replacement, and
shutdown. Replacement creates a new fid namespace and never implies replay of
the operation that detected failure.

FUSE already had these pieces split between `ClientSlot` and
`R9pFuse::reconnect`. The connection, client-slot replacement, reconnect lock,
and epoch are generic session mechanism. FUSE path-backed node rebinding,
open-handle replay classification, and kernel invalidation remain FUSE bridge
semantics.

The transparent namespace client already replaces failed referral sessions for
safe read-only path operations. Delimiter-terminated path reads need the same
mechanism so blocking observers do not retain a dead opened fid.

The reverse exporter is not a renewable client session. It owns the opposite
side of the connection: a bounded pool of outbound server streams, worker
phasing, session claims, and fresh server handlers. It should keep its own
pool-oriented reconnect loop.

Service registration is also distinct. It reconciles a governed `/srv` record
and its lease after reconnect. The r9p client session can supply a renewed
attachment, but only the registering service can decide to recreate or rewrite
the registration.

Terminal observation is an application cursor over the generic session.
r9p can reconnect, reattach, and reopen an exact read-only sequence path.
r9wm must retain the next terminal update sequence, accept an authoritative
screen resynchronization, and never replay ambiguous input.

## Effect

`ClientSlot` is replaced by `ClientSession`, which owns generic renewable
client-session state. FUSE and the session control runtime consume it. The
namespace client gains delimiter-terminated read-only path helpers with safe
failed-referral recovery. r9wm can use the same session replacement and resume
its exact terminal sequence without adding transport logic.

Reverse-export pooling, service registration reconciliation, FUSE rebinding,
and terminal cursor resumption remain separate because they recover different
state above the shared 9P attachment.
