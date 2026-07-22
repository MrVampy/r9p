# Reverse-Connect 9P Transport

Date: 2026-07-22.

## Question

How can a filesystem-owning host behind an inbound firewall publish an ordinary
9P tree without introducing SSH, a file synchronization lane, or
application-specific protocol machinery?

## Sources inspected

- `crates/auth/src/handshake.rs`, especially `authenticate_client` and
  `authenticate_server`
- `crates/auth/src/stream.rs`, especially `SecureStream::try_clone` and its
  `Read`, `Write`, and `ConnectionStream` implementations
- `crates/core/src/server/connection.rs` and
  `crates/core/src/server/file_tree_handler.rs`
- `crates/cli/src/commands/serve/runtime.rs`
- `crates/fs/src/lib.rs`, especially `LocalTree::open_with_config`
- `crates/core/src/blocking.rs`, especially `Client::connect_with_variant`
- [rathole client](https://github.com/rathole-org/rathole/blob/main/src/client.rs)
  and [server](https://github.com/rathole-org/rathole/blob/main/src/server.rs)
  source, especially its per-service control channel, requested data channels,
  pool, heartbeat, and exponential reconnect behavior
- [Chisel](https://github.com/jpillora/chisel), especially its authenticated
  reverse forwarding, keepalive, multiplexing, and exponential reconnect
  behavior
- [Cloudflare Tunnel](https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/),
  especially its outbound-only connector posture
- [Tailscale control and data planes](https://tailscale.com/docs/concepts/control-data-planes)
  and [DERP servers](https://tailscale.com/docs/reference/derp-servers),
  especially centralized coordination with direct data paths and explicit relay
  fallback
- [Plan 9 `srv`, `exportfs`, and `import` lineage](https://9p.io/wiki/plan9/9p_services_using_srv%2C_listen%2C_exportfs%2C_import/)

## Findings

- The Noise IK session layer already authenticates and encrypts an arbitrary
  TCP stream before 9P starts. Its `SecureStream` supports independent cloned
  read and write handles and is already an ordinary generic 9P connection
  stream.
- The generic server accepts any `ConnectionStream`; it does not require the
  9P server to have called `accept(2)` on the underlying TCP socket. Therefore
  the filesystem owner may connect outward and then serve 9P on that stream
  without changing a single 9P message.
- A broker can pair that authenticated outward stream with a loopback client
  connection by copying bytes bidirectionally. It does not parse messages,
  own fids, translate filesystem behavior, or acquire the exported authority.
- `LocalTree` remains the correct filesystem adapter. The reverse transport
  changes connection placement only; read-only versus writable behavior and
  path containment remain owned by the existing adapter.
- A bounded pool of one-session reverse streams fits existing one-connection
  9P server semantics and avoids inventing another multiplexing protocol.
- Rathole uses a pool of ordinary data channels behind one service identity,
  requests replacements as channels are consumed, and backs off reconnects.
  That supports a bounded pool and capped backoff here. Its separate control
  protocol is not needed because every r9p reverse stream is already a complete
  authenticated 9P session.
- Chisel demonstrates that multiplexing and keepalives are common when many
  arbitrary tunnels share one long-lived transport. r9p does not need that
  extra framing for the current one-session-per-stream contract. It can be
  added later only if measurements justify it.
- Cloudflare's connector confirms that outbound-only establishment is a normal
  way to cross an inbound firewall. Tailscale supplies the stronger system
  boundary: governance and addressing can remain centralized while the data
  path is direct, with relay represented as an explicit participant rather
  than hidden inside the authority.
- Plan 9's `srv` binds names to posted channels, and `cpu` can start a local
  `exportfs` after dialing a remote compute host. Listen and reverse-connect
  are therefore connection postures beneath the same 9P service abstraction,
  not different service contracts.

## Effect

The `r9p-reverse` runtime-adapter crate provides a generic authenticated broker
and reverse filesystem exporter. The protocol core, filesystem adapter, Vault
policy, and application admission remain unchanged.

The implementation now bounds queued streams, concurrent authentication, and
active bridges; keeps the consumer proxy loopback-only; discards observably
closed idle streams; exposes typed status counters; replenishes a consumed
stream without an artificial delay; and applies capped exponential reconnect
delay with deterministic worker phasing. The `r9p` CLI exposes the same broker
and exporter without adding registry semantics.

## Open questions

- A future need for long-lived multi-session stream multiplexing should be
  justified by measurements before adding a framing layer.
- Generic namespace addressing should resolve listen-backed and reverse-backed
  services to the same connection descriptor. Admission, lease, revocation,
  and relay selection belong to that namespace authority; the r9p transport
  must not absorb them.
