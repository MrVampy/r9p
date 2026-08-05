# One Session Credential, XX Only

Date: 2026-08-05

## Question

Client setup was O(N×M): every caller named every service it might be referred
to, and mapped each name to a local credential path. What in the source forces
that, and what has to go for one credential plus a named responder to be
sufficient?

## Sources Inspected

- `crates/session/src/authority.rs`
  - `AuthorityBindings::session_auth_config`
  - `contained_authority`
- `crates/session/src/client/namespace.rs`
  - `Client::connect_route` (referral connection construction)
  - `State.session_auth`
- `crates/core/src/export_descriptor.rs`
  - `AuthBoundary::p9any_domain`
  - `AuthBoundary::validate`
  - `validate_p9any_domain`
- `crates/auth/src/p9any.rs`
  - `negotiate_server`, `negotiate_client`
- `crates/auth/src/handshake.rs`
  - `client_xx`, `server_xx`, `certified_identity`
  - `verify_responder`
  - `authenticate_client_to`
- `crates/auth/src/config.rs`
  - `ClientConfig::read`, `ServerConfig::read`
  - `ServerConfig::authorize`
- `crates/auth/src/peercred.rs`
  - `TransportIdentity::transport_authorizes_uname`
- `crates/session/src/transport.rs`
  - `connect_stream`

## Findings

The map was restating the protocol. `AuthBoundary::p9any_domain` already
extracts the service name from a referral's `authority_boundary`, and under XX
`verify_responder` checks exactly that name against the responder's
certificate. Once the responder proves its own name, a per-service binding adds
nothing: the referral says who should answer and the certificate says who did.
`contained_authority` was the only part worth keeping — loopback, Unix sockets
and named network classes are boundaries in themselves, so they need no
credential at all.

IK is what made the map necessary. `ClientConfig` carried `server_key` because
the initiator has to know the responder's static before it can write message
one; that pin is per-service by construction, so the credential had to be
per-service too. XX transmits both statics inside the handshake, so the pin —
and with it the per-service split — disappears.

Two states are legal that a single `Option<PathBuf>` could not express. A root
reached over a Unix socket carries no responder but still needs a credential
for the services it is referred to; `hosts/m7/agent.nix` does exactly this
against `unix!/run/coordinator-namespace/9p`. Meanwhile a TCP root with a
credential and no responder must be an error, not a plaintext dial. Hence
`SessionAuthentication` pairing a validated `ClientCredential` with an optional
validated `ResponderName`: responder-without-credential is unrepresentable, and
`connect_stream` rejects the TCP case rather than downgrading.

`transport_authorizes_uname` was the subtle one. Deleting the attested seam
invited collapsing it to "the certified principal equals the requested uname",
which would let a certificate satisfy a consumer's admission check on its own.
A certificate binds which name a caller may ask for; it does not admit that
caller. Only `Local` — a transport that is already the trust boundary —
authorizes a uname by itself.

## Consequences

`AuthorityBindings`, `--authority-auth`, `r9p_front_bind_client_authority`,
peer lines, `server-key`, `authenticate_client`, `authenticate_server_attested`
and the whole IK path are deleted rather than deprecated. Front ABI 21 → 22.
Every dial that authenticates now states the responder it expects: a root from
its caller, a referral from its authority boundary.
