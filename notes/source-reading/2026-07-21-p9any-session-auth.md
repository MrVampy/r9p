# P9any-authenticated encrypted 9P sessions

Date: 2026-07-21

## Question

How should r9p authenticate and encrypt a remote 9P door without depending on
a network-level tunnel, while retaining Plan 9's separation between 9P,
authentication protocol negotiation, and stream protection?

## Sources inspected

- `refs/vault/refs/9front/sys/man/5/attach`
- `refs/vault/refs/9front/sys/man/6/authsrv`
- `refs/vault/refs/9front/sys/src/lib9p/auth.c`
- `refs/vault/refs/9front/sys/src/cmd/auth/factotum/p9any.c`
- `refs/vault/refs/9front/sys/src/cmd/auth/factotum/p9sk1.c`
- `refs/vault/refs/9front/sys/src/libauthsrv/authpak.c`
- `refs/vault/refs/9front/sys/src/cmd/tlsclient.c`
- `refs/vault/refs/9front/sys/src/cmd/tlssrv.c`
- `refs/vault/refs/inferno-os/appl/cmd/9export.b`
- `refs/plan9port/src/libauth/fsamount.c`
- `refs/plan9port/src/cmd/auth/factotum/p9any.c`
- `crates/core/src/server/handlers.rs`
- `crates/core/src/blocking.rs`
- `crates/core/src/multiplex/client.rs`
- `crates/session/src/transport.rs`
- Snow source at upstream revision `8ac60f51cfe3e010c84f0a454cc575ad9204fa12`,
  especially `src/handshakestate.rs` and `src/stateless_transportstate.rs`
- The normalized `Cargo.toml` shipped in the Snow `0.10.0` crate

## Findings

- `Tauth` and the auth fid are the 9P mechanism for carrying an authentication
  conversation. The authentication protocol itself is intentionally outside
  the 9P wire definition.
- Plan 9 uses `p9any` as a versioned negotiator. A server offers protocol and
  domain pairs; a client selects one; the selected provider establishes the
  authenticated identity and a shared secret.
- 9front's `tlsclient -a` and `tlssrv -a` run p9any before protecting the
  application stream. They use the resulting secret as the TLS PSK. Inferno's
  `9export` follows the same authenticate-then-protect composition.
- `dp9ik` is a substantial password-authenticated protocol involving AuthPAK,
  tickets, an authentication server, and factotum state. A partial reimplementation
  would not be dp9ik and must not be described as wire-compatible with it.
- r9p already decodes and admits `Tauth`, auth qids, and auth-bearing `Tattach`,
  but its production clients always attach with `NOFID`. Its export `auth`
  value was only an operator assertion; it did not authenticate a connection.
- Noise IK provides a compact public-key provider with mutual static-key
  authentication. The selected suite is X25519, ChaCha20-Poly1305, and BLAKE2s.
  Snow's stateless transport mode permits independent ordered read and write
  record counters, which fits r9p's cloned blocking stream model without
  serializing reads behind writes.
- Snow `0.10.0`'s published `std` feature also activates crypto providers not
  used by the selected suite. The dependency therefore disables default and
  `std` features and enables only X25519, ChaCha20-Poly1305, BLAKE2, and secure
  randomness; r9p-auth itself remains a standard-library crate.

## Effect on the implementation

- Add a reusable `r9p-auth` crate above the runtime-neutral protocol core.
- Negotiate `noise-ik@<domain>` with the p9any version 2 wire shape, then run
  Noise IK and carry 9P over bounded authenticated-encryption records.
- Pin the server public key on clients. On the server, bind each admitted client
  public key to an explicit set of allowed 9P usernames.
- Bind the authenticated username into the core server session so a later
  `Tauth` or `Tattach` cannot claim a different `uname`.
- Replace declarative network-tunnel descriptor classes with the enforced
  `p9any:noise-ik@<domain>` class. Do not retain aliases.
- Keep Unix sockets and loopback connections as explicit local trust paths.
- Keep Vault admission, namespace policy, and governance outside r9p; Vault
  receives the verified session identity before applying those policies.

## Interoperability boundary

The new provider uses the standard p9any negotiation shape but is not a dp9ik
implementation and is not accepted by an unmodified 9front factotum. P9any is
deliberately provider-extensible, so a complete dp9ik provider can be added
later if direct factotum interoperability becomes a concrete requirement. The
current contract names `noise-ik` honestly and carries no dp9ik compatibility
claim.

## Open questions

- Whether a future multi-operator deployment needs a full factotum/auth-server
  lane and genuine dp9ik interoperability.
- Whether enrollment should later become a governed namespace operation rather
  than a host-composition public-key map.
