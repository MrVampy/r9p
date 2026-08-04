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
*learns* a principal's name instead of asserting it. Server configs would then
list keys (or a signing authority) and authorization would be written against
names the credential itself carries.

Design questions that belong to this repository, not to the host flake:

- Does the name live in a signed credential issued by an authority, or is it
  bound to the key at generation and self-asserted? The first prevents
  disagreement; the second is cheaper and still removes the per-server list.
- If an authority signs, is it coordinator — reversing the original cost
  decision — or a small local signer analogous to `nebula-cert`, which is what
  made the mesh case cheap?
- Migration must tolerate a mixed fleet: some hosts naming keys locally while
  others read names from credentials, since a simultaneous fleet-wide switch is
  exactly the failure mode this is meant to remove.

## Interim, on the host-flake side

A `principals.nix` declaring each identity once and generating every `peer`
line and authorization list is planned there. It removes the duplication but
not the underlying gap: it is a **cache** that keeps relying parties agreeing,
not a binding. If a name ever becomes signed material, that file is the input
you sign from rather than the runtime truth.
