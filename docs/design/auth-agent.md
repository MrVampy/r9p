# The auth agent

Status: deferred with explicit activation triggers. The certificate and Noise
XX transport are complete without this agent. This document reserves the
long-term key-custody boundary so a later implementation solves a measured
problem instead of becoming a second authentication stack.

## Current position

The deployed authentication model is already exteriorized where distributed
agreement matters:

- an offline Ed25519 root signs a principal name, role groups, validity window,
  and X25519 static public key;
- Noise XX proves possession of that same X25519 key on every connection;
- each peer verifies that the certificate key is the key that completed the
  handshake;
- the dialling peer also verifies the responder name it requested; and
- applications receive a verified transport identity and apply their own
  admission policy.

The earlier duplication problem is also closed. `SessionAuthentication`
contains one credential and its responder, referrals derive their responder
from that value, and `ConnectionAuthentication` makes an unauthenticated
connection an explicit `Unauthenticated` choice rather than an omitted field.
Adding a local process merely to centralize those typed values again would not
improve the system.

One security property remains available only through an agent: the long-term
X25519 private key can stay outside every client and server program address
space. That is the reason to build this later.

## What a future agent owns

The future component is a local authentication and key-custody agent inspired
by factotum. It owns one OS identity's long-term authentication material and is
reached through a local, kernel-authenticated boundary. It may eventually own:

- X25519 static private-key operations;
- the currently active public certificate and trusted roots;
- atomic certificate and key rotation;
- renewal state and signed revocation state; and
- the audit facts needed to explain which identity material authenticated a
  connection.

It does not own namespace admission. coordinator continues to address,
register, govern, and admit services based on the certified identity returned
by r9p. An authentication agent must not become an online CA, a network proxy,
a fleet keystore, or a coordinator subsystem.

The preferred deployment boundary is local and per OS identity. Unix peer
credentials admit the caller. Whether one process hosts several isolated
per-identity instances is an operational choice; one caller must never gain a
second identity merely because both are present on the same host.

## The real Noise boundary

Traditional factotum lets a program carry protocol messages between its remote
peer and `/mnt/factotum/rpc`. The conversation ends with authentication
information and, for some protocols, a negotiated secret. That is useful
lineage, but it does not by itself specify r9p's handoff.

The current r9p implementation converts Snow's `HandshakeState` into an opaque
`snow::StatelessTransportState`. `SecureStream` retains that state and uses it
to encrypt or authenticate every subsequent 9P record. Therefore an agent
cannot simply run the handshake, return "authenticated", and leave the data
path. The consumer needs the directional transport keys or an equivalent live
transport object.

Three possible boundaries follow:

- Keeping the Snow transport state in the agent would require a local RPC for
  every encrypted record. That makes the agent a data-plane proxy and is
  rejected.
- Exporting a bounded pair of short-lived directional session keys would let
  the consumer own record encryption while the long-term key stays in the
  agent. Snow 0.10 can expose the raw split only behind `risky-raw-split` and
  does not construct a `StatelessTransportState` from it, so r9p would need a
  small, reviewed transport-key seam rather than serializing Snow internals.
- Letting the consumer run the handshake while the agent performs only static
  X25519 operations is the narrowest custody boundary. Snow's current
  `CryptoResolver` and `Dh` interfaces do not expose a clean external-static-DH
  slot: the builder requires `local_private_key`, and `Dh` includes `set` and
  `privkey` as well as `dh`. This needs an upstreamable Snow extension or an
  equally explicit r9p adapter, not reliance on resolver call order.

The target is the last boundary if it can be made explicit in Snow: the local
program owns the Noise transcript, ephemeral key, derived session state, and
record transport; the agent performs only operations requiring the long-term
static private key and supplies public certificate material. If that seam
cannot be made cleanly, exporting short-lived split keys is the bounded
fallback. A data-plane proxy is not.

Short-lived session keys entering the authenticated endpoint process are not a
failure of key custody. That process already sees the plaintext session. The
property being protected is that compromise of one process does not disclose a
reusable long-term identity key.

## Relationship to certificate lifecycle

The agent and the issuer solve different problems:

- the issuer decides which name, groups, key, and validity window may be
  certified;
- the local agent protects the certified key and may request or install a
  renewal; and
- r9p proves the resulting identity on a connection.

The offline root never moves into the agent. A future online issuer uses a
constrained intermediate and remains a separately registered namespace
participant. coordinator may govern access to it but does not contain issuance
logic. The lifecycle is specified in
[`certificate-lifecycle.md`](certificate-lifecycle.md).

## Activation triggers

Implement the agent when at least one of these becomes current work:

- short-lived certificates require unattended renewal;
- signed revocation state must be applied consistently on each host;
- one host needs several independently selectable principals without copying
  their private keys into every consumer;
- a concrete threat model requires long-term private keys to stay outside
  service processes; or
- another authentication omission reaches production after the explicit
  `ConnectionAuthentication` cutover.

Fleet size by itself is not a trigger. Neither is the aesthetic fact that more
than one program calls the shared r9p authentication library.

## Implementation gates

When a trigger fires, implementation starts only after these gates are closed:

1. Prove the Snow handoff with a small source-level prototype. Choose an
   explicit external-static-DH interface or a reviewed raw-split transport
   seam. Do not start with a daemon and discover the cryptographic boundary
   later.
1. Specify the local 9P object and its Unix peer-credential admission. The
   protocol must carry opaque operation handles and bounded binary values, not
   private keys or caller-selected filesystem paths.
1. Specify crash, restart, cancellation, and backpressure semantics. An agent
   outage may prevent new sessions but must not damage established sessions.
1. Prove identity isolation with at least two local principals and negative
   cross-principal tests.
1. Cut one client and one responder over, then delete their private-key loading
   paths. Do not retain an automatic direct-key fallback.
1. Complete the cut only when all production consumers no longer read the
   long-term private key themselves.

The deletion test remains the acceptance test. If the new agent does not remove
long-term private-key access from consumers, it is an additional copy rather
than exteriorization.

## Prior art and limits

Plan9port factotum's paired write/read RPC is the reference for a local,
file-shaped authentication conversation. It proves that the application can
carry peer messages without giving the agent the network connection. It does
not prove that an opaque Noise transport state can cross that boundary.

`session-proxy` and `reverse-broker` demonstrate local credential-bearing
helpers, but both sit at different boundaries. They must not be renamed or
expanded into this role. The future auth agent is new only if the key-custody
property is worth its hard host dependency.
