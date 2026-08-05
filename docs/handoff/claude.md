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
`crates/cli/src/commands/cert.rs`) with `root`, `sign`, `print` and `verify`,
and **wired into the handshake** — a certificate now names a session. Nothing
in the fleet is configured to use it yet; see "Rolling it out" below.

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

## The transport, as built

`ClientConfig` takes an optional `certificate`; `ServerConfig` takes `root`
lines and may hold peers, roots, or both. `ServerConfig::new` keeps its old
signature and semantics, so nothing outside had to change;
`new_with_roots` is the wider constructor.

The certificate rides in the first Noise message payload, where the bare
principal used to go, behind a leading `0x00`. That byte is a free
discriminator: `validate_principal` rejects NUL and every control byte, so a
legacy payload can never begin with it and the two forms need no version
negotiation.

The load-bearing check is that **the certificate was issued for the key that
completed this handshake**. Certificates are public; without that check anyone
holding a copy could present it. Noise proves possession of the key, and the
check ties the signed name to that same key. `a_certificate_cannot_be_replayed_by_another_key`
forces a stolen certificate past the client-side guard to prove the server
refuses it too.

`PeerIdentity` gained `groups()` and `certified()`. A peer-line identity has no
groups, so authorization written against groups denies a legacy session rather
than silently widening it.

## Rolling it out

**Servers before clients, and it is not merely convention.** A server predating
this reads the first payload into a 255-byte buffer, which a certificate
overflows. `an_undersized_server_buffer_refuses_rather_than_truncating` proves
snow errors rather than delivering a prefix — otherwise an old server could
read part of signed material as a principal. So:

1. Deploy servers carrying `root` lines beside their existing `peer` lines.
   `peer_lines_keep_working_while_roots_are_configured` covers this state.
2. Verify legacy sessions still authenticate everywhere.
3. Issue certificates and cut clients over one at a time.
4. Remove `peer` lines last.

A new client against an old server fails; an old client against a new server
works. That asymmetry is what makes the order safe.

Not yet done on the host-flake side: no root exists in sops, no host declares
`root` or `certificate` lines, and nothing consumes `groups()` for
authorization yet — `operators = [ ... ]` lists are still name lists.

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
