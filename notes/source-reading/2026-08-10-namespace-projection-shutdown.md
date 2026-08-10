# Namespace projection shutdown wake

Question: how should a private namespace projection stop when its published
Unix socket is deliberately inaccessible to the projection owner?

Sources inspected:

- `crates/session/src/projection/mod.rs`: projection accept, active-session,
  and drop lifecycle.
- `crates/session/src/client/namespace/operations.rs`: logical namespace
  client shutdown across root and referral sessions.
- `crates/core/src/multiplex/client.rs`: shared transport shutdown and pending
  call failure.
- Agent `host_supervisor/runtime/server/namespace_projection.rs`: the runtime
  supervisor assigns the published socket to the selected harness identity
  while retaining projection custody without `CAP_DAC_OVERRIDE`.

Finding:

The projection previously woke its blocking accept loop by connecting to its
own published socket. That made shutdown depend on the socket's external access
policy. Once a capability-minimal owner transferred the `0600` socket to a
different execution identity, the reconnect was denied and drop blocked while
joining the acceptor.

Decision:

Projection shutdown uses a private Unix stream pair as an event-driven wake
descriptor. The acceptor blocks on the listener and wake descriptor together.
Drop marks shutdown, interrupts active clients, closes the private wake side,
joins the acceptor, and then waits for active sessions to release. Public
socket ownership and permissions no longer participate in internal lifecycle
control.

This belongs in the session runtime adapter, not Agent or the 9P protocol core.
It is useful to any private projection whose publisher and consumer are
different Unix identities, and it changes no 9P wire semantics.
