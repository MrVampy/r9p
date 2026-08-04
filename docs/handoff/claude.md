# Claude Handoff

Last updated: 2026-08-04.

## Session auth binds keys but not names

Raised from operating the r9 mesh, not from reading r9p in isolation. It is a
design question, not a defect report.

`r9p-session-auth.v1` server configs carry:

```
peer <public-key> <name>
```

The key authenticates. The name does not — it is a label each relying party
asserts locally, and it is then what authorization is written against
(`operators = [ "..." ]`, `admittedCallers`, credentials' `allowed_services`).

So a name is treated as a global fact while being stored as per-server config.
Two consequences follow, and the second is the expensive one:

- **Nothing makes relying parties agree.** One host may call a key
  `codex.interface` while another calls the same key `operator`. Both configs
  are valid; the system runs; the disagreement surfaces only when an
  authorization list written against one name meets a session authenticated
  under the other.
- **Renaming a principal is a fleet-wide edit.** In the operator's setup the
  laptop identity appears ~18 times across 12 files in the host flake, plus
  credentials and dependencies, plus live policy state in the Credentials
  store. Every one must change simultaneously, because the client presents the
  name and the server maps the key to it — a partial rename is broken auth, not
  a degraded mode.

## The same system already demonstrates the alternative

Nebula, which the mesh runs on, puts the name **inside the signed
certificate**: `nebula-cert sign -name tuxedo`. Peers learn the name during the
handshake. There is no peer-name list to maintain, relying parties cannot
disagree, and a rename is one re-signing rather than an edit everywhere.

That difference is observable operationally: rotating and renaming Nebula
identities across four hosts was routine, while renaming one r9p principal is
the reason this note exists.

## Why it is the way it is

Deliberate, per the operator: coordinator owning this was too expensive for
what session auth needed to do, so `r9p-session-auth` took the cheap shape —
raw keys plus locally-asserted names, essentially `authorized_keys`. That was a
reasonable trade. The cost did not vanish, though; it moved into configuration
duplication and turned naming into a distributed agreement problem.

## What would close it

Carry a name in signed material, the way Nebula certs do, so a relying party
*learns* a principal's name instead of asserting it. Server configs then hold a
signing authority's public key and no `peer` lines.

## Decision: an offline signer, not a service — **the signer now exists**

A small local tool, analogous to `nebula-cert`. Root key in sops, run by the
operator, no daemon. Coordinator does not take this on, reversing nothing about
the original cost decision.

Shipped as `r9p cert` (`crates/auth/src/cert.rs`,
`crates/cli/src/commands/cert.rs`) with `root`, `sign`, `print` and `verify`.
The **transport does not consult it yet** — see "What remains" below. Issuing a
certificate currently changes nothing about who may connect.

Implementation notes worth keeping:

- **Two key types, not one.** Session keys are X25519 Noise statics and cannot
  sign, so the root is Ed25519 and signs over the subject's X25519 public key.
  Nebula splits them the same way. `ed25519-dalek` is pinned to `=2.2.0`
  specifically because 3.0 pulls a second `curve25519-dalek` alongside snow's;
  2.x shares it, so the tree holds one copy of that field arithmetic.
- **Signed bytes are canonical, not textual.** The signature covers a
  length-framed encoding built from the parsed fields, so reformatting cannot
  change what was signed and no field boundary can shift — `name "a" group "b"`
  cannot be re-read as `name "ab"`. There is a test for exactly that.
- **Validity is whole Unix seconds.** No calendar library in the trust path;
  `print` reports `expires_in_seconds`, matching the mesh service's
  `certificate_ttl_seconds`, which is the form a threshold alert wants.
- **Names and groups reject whitespace**, which `validate_principal` permits.
  They live on whitespace-delimited lines, so permitting it would break the
  round trip rather than be caught.

`r9p cert` **consumes** `auth-keygen` rather than replacing it.
`auth-keygen` (`crates/cli/src/commands/auth_keygen.rs`, `crates/auth/src/key.rs:139`)
generates a Noise static pair on the host that will use it, converges on re-run
— derives a missing public key, verifies a mismatched one, refuses to replace a
public key whose private is gone — and prints `public_key\t<hex>`. That line is
already a certificate request minus the request. `cert sign` takes the public
half, so the private key never leaves the machine that made it, which is better
than the mesh's own posture: `nebula-cert sign` mints the pair on the operator's
laptop and ships it through sops.

The credential carries:

- `name` — learned by the relying party, not asserted.
- `group` (repeatable) — so authorization stops enumerating principals. The
  mesh firewall admits on group/cidr rather than listing peers;
  `operators = [ "codex.interface" ]` should become "carries group `operator`".
  Centralizing the list removes duplication; carrying groups removes the list.
- `not-before` / `not-after` — session keys otherwise have no lifetime, so "how
  long is this good for" answers "forever, including after it leaks".

## What remains: the transport

The handshake still authorizes through `peer` lines. What has to change, from
reading `crates/auth/src/handshake.rs`:

- The client's **first Noise message payload already carries the principal**
  (`handshake.rs:100`), and the server reads it back and calls
  `config.authorize(public_key, &principal)` (`handshake.rs:169`). That payload
  is where a certificate goes.
- **A discriminator is available for free.** Principals are validated UTF-8
  with no control bytes, so a payload whose first byte is `0x00` can never be a
  legacy principal. That makes old and new payloads unambiguous without a
  version negotiation.
- **The server's read buffer must grow.** It is `MAX_PRINCIPAL_BYTES` (255) at
  `handshake.rs:157`, and a certificate is larger. Read into a
  `MAX_NOISE_MESSAGE_BYTES` buffer and bound by payload kind instead.
- **`ServerConfig` needs a root instead of, or beside, peers.** It currently
  *requires* at least one peer (`config.rs:89`); a cert-only server has none.
- **Migration order is servers first, then clients.** A new client against an
  old server fails, because the old server cannot parse the payload or size the
  buffer. Both must be accepted for a period, which is the mixed fleet this
  note has asked for from the start.
- **`preauthorized` already models the distinction** the cert path needs:
  `authenticate_server_attested` admits an unlisted key and defers to the
  application. A certificate-authenticated session is a third case — the name
  is proven, not asserted and not deferred.

## What it does not close

- **Capability policy.** Groups do not express "wsl may open terminal sessions
  but not agent runtimes". Minting a group per capability recreates the scatter
  somewhere else.
- **Revocation.** CAs are weak at it; short lifetimes are the practical answer
  and those need an online issuer. Until then, revoking early means re-signing
  downstream.
- **Recorded state.** ~72 journal transactions in the Credentials store name
  the principal directly. That is data, not config, and still needs migrating.

## If an online issuer is later wanted

Recorded because the bootstrap now has an answer it lacked before. A node
reaching such a service is already on the Nebula overlay, having handshaken
against a CA-signed certificate carrying a name and groups — session identity
can be *derived from* mesh identity rather than invented. Shape would be
two-tier: root offline in sops as above, plus an intermediate online that may
only mint short-lived leaves for declared names.

The trigger is churn or short lifetimes, not fleet size. Four hosts changing
twice a year do not justify a signing daemon, and an online signer that can
mint any identity is strictly worse to compromise than anything it issues.

## The host-flake side

`principals.nix`, declaring each identity once, is planned there. Before
signing exists it is a **cache** that keeps relying parties agreeing, not a
binding. After, it is the input `r9p-cert` signs from — the role
`nebula/topology.nix` plays for `nebula-issue-cert`.

Migration must tolerate a mixed fleet: some hosts naming keys locally while
others read names from credentials, since a simultaneous fleet-wide switch is
exactly the failure mode this is meant to remove.
