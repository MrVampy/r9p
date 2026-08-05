# The auth agent

Design note. Nothing here is built. It follows from certificates landing and
from the principle that says the way they landed is half wrong.

## The problem

Certificates removed both halves of the N×M matrix: a server no longer lists its
clients (roots), and under XX a client no longer pins its servers (mutual
certificates, the responder's name checked against what the caller asked for).
That part is done and deployed.

What it did *not* do is move the machinery out of the programs. Every program
that opens a session now reads a private key, holds a certificate, knows what a
root is, and runs the handshake itself.

The evidence is not theoretical. Adding one field — the expected responder name
— required threading `auth_domain` through eight `ConnectionConfig` sites:
`cli/io.rs`, `cli/commands/con.rs`, `session/transport.rs`, the namespace
referral path, `control/mod.rs`, `fuse/config.rs`, `front/abi/client.rs`,
`beam-port/lib.rs`. Five of those were first patched to `None` to satisfy the
compiler, which compiled cleanly and silently disabled the feature until the
end-to-end test failed with "names no service".

That is capability existing eight times, and failing quietly in the copies.

## Why this is the wrong shape

The house principle is exteriorization: a component exposes itself and stays
dumb, capability lives outside and composes through the namespace. The opposite
— interiorization — makes the component the platform and duplicates every
capability inside it. The waist already exists; a second one inside a process
violates the architecture outright.

By that test the identity *model* is exteriorized correctly — names travel as
signed data, coordinator addresses without being able to impersonate — while the
identity *machinery* is interiorized in every leaf.

Plan 9 answered this long ago and the answer is not certificates. It is
factotum: authentication lives in an agent that owns the keys and speaks the
protocols on behalf of programs, exposed as files. Programs never see a key,
never parse a certificate, never learn what a root is.

## What it is

A **local** agent holding **one principal's own** keys, reached over a unix
socket and admitted by peer credentials — `peercred.rs` already has
`UNIX_SAME_USER_SUBJECT`. Compromising it compromises exactly one identity,
which is what compromising the process already gets you. No new concentration of
trust; that is the test a second waist fails.

It relays rather than proxies. The program carries handshake bytes between the
remote peer and the agent and receives the negotiated result; the agent never
sits on the data path and never sees session traffic. No fd passing. The
"must not proxy traffic" rule binds the agent, not only coordinator.

It is exposed as files, or it is a library with extra steps. The idiom exists:
single-fid request/response, the way `r9p rpc` and `/entries/ctl` already work.

## What it is not

- **Not an issuance service.** The root stays offline and the agent mints
  nothing. An online CA was considered and refused; that stands.
- **Not a network service.** Local socket only. A networked auth service would
  re-centralize precisely what certificates just decentralized.
- **Not a fleet keystore.** It holds its own principal, not everyone's.
- **Not coordinator.** Coordinator remains addressing, registration, governance
  and admission, holding no keys and running no handshakes.

## The test: what it deletes

If it does not delete these, it is a ninth copy rather than exteriorization.

- `--auth-domain` and `--auth-config` from every program
- private-key handling from all eight `ConnectionConfig` sites
- each program's knowledge of certificates, roots, and expiry
- rotation touching eight places instead of one

## Open questions

- **Per-user or per-service-user?** m7 runs services under several identities.
  One agent per identity means more copies; one agent for all of them is a local
  keystore holding several principals, which is a smaller version of the thing
  refused above. Leaning per-identity, because the blast radius argument is what
  makes the design safe at all.
- **Does it own the server side too?** Today `terminal-m7` reads its own key and
  runs the responder handshake. The symmetric answer is that it should not, and
  that is a larger change than the client side. The client side is worth doing
  first regardless.
- **What happens when the agent is down?** Sessions cannot be established. That
  is a new failure mode and wants supervision, not a fallback path — a fallback
  would restore the interiorized copy it exists to delete.

## Migration

Forward-only, like the certificate rollout, and in the same shape that worked:
agent first, then one program cut over and proven, then the rest, then delete
the flags and the key handling. The certificate format, the offline root, XX,
and the referral name check all survive unchanged. What moves is who executes
them.

## Prior art in this repository

`session-proxy` and `reverse-broker` are already agents holding credentials on
behalf of other programs. This is that pattern applied one layer down.
