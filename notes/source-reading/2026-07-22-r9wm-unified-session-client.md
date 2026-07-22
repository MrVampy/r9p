# r9wm unified session client

Date: 2026-07-22

## Question

Can r9wm obtain Unix, plain TCP, and authenticated remote 9P sessions entirely
through r9p, without selecting socket or authentication machinery in r9wm?

## Sources inspected

- `crates/core/src/multiplex/client.rs`
- `crates/core/src/multiplex/mod.rs`
- `crates/auth/src/handshake.rs`
- `crates/auth/src/stream.rs`
- `crates/cli/src/transport.rs`
- `crates/session/src/client.rs`
- `crates/session/src/connection_config.rs`
- `crates/session/src/transport.rs`
- r9wm `crates/terminal-contract/src/client.rs`

## Findings

The r9p session crate already owns the reusable transport union and endpoint
selection that r9wm needs. `ConnectionConfig` selects TCP, `unix!`, or
`namespace!`; an optional client auth configuration upgrades TCP to the
p9any/Noise IK `SecureStream`; and `Client` wraps the resulting stream in the
same multiplexed 9P client used by the rest of r9p.

r9wm was already using r9p for wire and multiplexing, but it fixed its terminal
client to `MultiplexedClient<TcpStream>` and called `connect_tcp` directly.
That leaked transport selection into the consumer and made Unix and
Noise-authenticated sessions unavailable there.

The reusable session client lacked only public wrappers for two mechanisms the
terminal projection needs: an unbounded `read_full` for a blocking namespace
subscription file, and connection-wide `shutdown` to cancel that blocked read
during detach. Both mechanisms already existed on the underlying r9p
`MultiplexedClient`; the session crate now exposes them without adding a new
transport or lifecycle abstraction.

## Effect

r9wm can replace its concrete TCP client with the r9p session client and add an
optional auth-config coordinate. It should not implement endpoint parsing,
Unix sockets, Noise authentication, or transport erasure itself.

## Open questions

None for the terminal-attacher cutover. Reconnect policy remains a consumer
choice built from r9p session primitives rather than an implicit retry of
state-changing terminal operations.
