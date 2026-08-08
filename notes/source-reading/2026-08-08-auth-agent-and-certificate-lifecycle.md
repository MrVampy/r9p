# Auth agent and certificate lifecycle boundary

Date: 2026-08-08

## Question

Is a complete factotum-shaped agent the next step after the certified Noise XX
cutover, and what certificate-lifecycle boundary should exist before that agent
is implemented?

## Sources inspected

- `crates/auth/src/handshake.rs`
- `crates/auth/src/stream.rs`
- `crates/auth/src/config.rs`
- `crates/auth/src/cert.rs`
- `crates/auth/src/key.rs`
- `crates/session/src/connection_config.rs`
- Snow 0.10.0 `src/builder.rs`
- Snow 0.10.0 `src/handshakestate.rs`
- Snow 0.10.0 `src/stateless_transportstate.rs`
- Snow 0.10.0 `src/resolvers/mod.rs`
- Snow 0.10.0 `src/types.rs`
- `refs/plan9port/src/cmd/auth/factotum/rpc.c`
- `refs/plan9port/src/cmd/auth/factotum/p9any.c`
- `refs/plan9port/src/libauth/auth_proxy.c`
- `notes/source-reading/2026-07-21-p9any-session-auth.md`
- `docs/handoff/claude.md`, certificate and optionality follow-up sections

## Findings

- The live identity mechanism is complete: mutual Noise XX certificates, key
  binding on both peers, responder-name verification, validity verification,
  and no bare-principal path.
- The earlier configuration duplication and silent omission are no longer
  evidence for an agent. `SessionAuthentication` consolidates credential and
  responder selection, while `ConnectionAuthentication::Unauthenticated`
  makes declining authentication explicit and greppable.
- Programs still load the long-term X25519 private key. Keeping that key out of
  their address spaces is the unique property a future local agent adds.
- `finish_handshake` returns `snow::StatelessTransportState`, and
  `SecureStream` retains it for every record's `read_message` and
  `write_message`. An agent cannot return only a verified identity and leave
  the data path.
- Snow's raw directional split is feature-gated as `risky-raw-split`; Snow does
  not expose a constructor that turns those keys back into a
  `StatelessTransportState`.
- Snow's custom `CryptoResolver` is close to a remote-static-DH seam but is not
  one. `Builder` requires a `local_private_key`, and the `Dh` trait exposes
  `set` and `privkey` as well as `dh`. Distinguishing the static and ephemeral
  resolver instances by call order would be fragile.
- Plan9port factotum provides a paired write/read RPC where the program carries
  remote protocol messages. Its `authinfo` may return a negotiated secret. It
  validates the local-agent shape but does not specify how r9p transfers two
  directional Noise transport keys or an opaque Snow state.
- A data-plane encryption proxy would make the agent a latency and availability
  dependency for every 9P record. It is the wrong boundary.
- The preferred future seam is an explicit external static X25519 operation:
  the endpoint process owns the transcript, ephemeral key, derived transport
  state, and record stream; the agent owns the long-term static operation and
  public certificate selection. A bounded raw-split handoff is the fallback if
  Snow cannot expose that seam cleanly.
- Certificate issuance, local key custody, connection authentication, and
  service admission are four distinct responsibilities. Factotum addresses
  custody and protocol execution, not issuance or admission.

## Effect on design

- Rewrite `docs/design/auth-agent.md` as a deferred design with explicit
  activation triggers and a source-backed Noise handoff prerequisite.
- Add `docs/design/certificate-lifecycle.md` to separate the current
  operator-signed phase from future renewal, revocation, intermediate issuance,
  key rotation, and root rotation.
- Keep the offline root out of both the local agent and coordinator.
- Do not implement a daemon until the Snow boundary is proved and a lifecycle
  trigger makes the hard host dependency worthwhile.

## Open questions

- Will Snow accept an upstream external-static-DH interface, or should r9p own
  a narrowly reviewed transport state built from the Noise raw split?
- Should the local agent be one process per Unix identity or one process with
  kernel-enforced isolated identity instances?
- What leaf lifetime classes and renewal thresholds should the deployment
  declare once an online intermediate exists?
- What exact signed revocation snapshot gives fast individual-key revocation
  without putting an online lookup in every handshake?
