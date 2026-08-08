# Certificate lifecycle

Status: lifecycle design. The mutual-certificate transport and certified-name
admission cutover are complete. Issuance remains operator-driven, the Ed25519
root remains offline, and no online renewal or revocation service exists.

## Boundary

r9p owns the generic certificate format, signature and validity verification,
binding to the X25519 key that completes Noise XX, and the authenticated
transport identity returned to its caller.

r9p does not decide:

- which principals or groups may be issued;
- how long a deployment's certificates live;
- whether a principal is admitted to a namespace or service operation;
- where an issuer runs; or
- how host policy installs new material.

Those are deployment and service-policy decisions. coordinator governs access
to registered participants but contains no certificate-issuance logic. An
issuer, if introduced, is an ordinary registered service with its own narrow
policy.

## What is complete now

The current mechanism has one clear trust statement:

1. An offline Ed25519 root signs a canonical certificate body containing a
   principal name, stable groups, validity interval, issuer, and X25519 static
   public key.
1. Noise XX proves possession of the corresponding X25519 private key.
1. Each peer verifies the signature, validity interval, and equality between
   the certified key and the key that completed the handshake.
1. The initiator additionally verifies that the certified responder name is
   the service name it requested.
1. The application admits or denies the resulting certified identity.

There is no IK, responder-key pin, raw-key subject, bare-principal path, or
peer-line fallback in the current contract.

## Current operator-driven phase

The present phase is deliberately simple:

- each X25519 key pair is generated on the host that uses it;
- only the public key leaves that host for signing;
- the operator uses `r9p cert sign` with the offline root;
- Nix installs the public certificate and root while preserving the host-local
  private key; and
- a restart trigger reloads services whose public authentication config
  changed.

Certificate lifetime and renewal threshold are deployment policy. They must be
declared centrally with the principal inventory rather than chosen ad hoc per
invocation. r9p continues to accept exact Unix-second validity windows and does
not hard-code a fleet policy.

This phase remains correct while renewal is infrequent enough for an operator
event. Long certificate lifetimes are a conscious exposure window, not a
substitute for lifecycle machinery.

## Lifecycle states

For one principal, the durable lifecycle is:

1. `Declared` - policy names the principal, allowed groups, key owner, and
   lifetime class.
1. `KeyReady` - the principal host has generated the private key and published
   only its public half for issuance.
1. `Issued` - an authorized issuer has signed an exact name, group set, public
   key, and validity interval.
1. `Installed` - the host has atomically paired that certificate with the
   matching private key and trusted issuer set.
1. `Active` - new sessions present it; already established sessions remain
   bound to the identity they authenticated at connection time.
1. `RenewalDue` - a deadline derived from `not-after` wakes the renewal path.
1. `Superseded` - a newer certificate or key pair is active for new sessions.
1. `Expired` or `Revoked` - it cannot establish a new accepted session.

The clock transition into `RenewalDue` is a deadline, not polling. Certificate
installation, issuer policy changes, and revocation publication are events.

## Renewal

Renewal normally keeps the X25519 key and issues a new certificate for the same
name and groups. A renewal request carries:

- the current certified identity or another admitted bootstrap identity;
- the subject public key;
- the requested lifetime class; and
- an idempotent request identity.

The issuer derives the permitted name and groups from its own policy. It never
trusts caller-supplied name or group values merely because the current transport
is authenticated. A renewed certificate is public output.

Installation is an atomic public-material replacement. New connections use the
new certificate. Existing encrypted sessions do not renegotiate and are not
interrupted merely because a certificate was renewed.

If renewal fails, the last valid certificate remains active until its exact
expiry. The failure is published as health state with the remaining validity
window. There is no silent extension and no fallback to an expired
certificate.

## Key rotation

Key rotation is distinct from certificate renewal:

1. Generate a new X25519 private key inside the principal's custody boundary.
1. Obtain a certificate for its public half under the same declared identity.
1. Verify the new certificate and key pairing locally.
1. Atomically switch the active key and certificate for new sessions.
1. Retain the previous pair only for the bounded rollback-free cutover window
   needed to confirm new sessions, then destroy the retired private key.

Admission policy continues to name the certified principal or group, not the
rotating public key. A key rotation therefore does not rewrite service policy.

The bounded overlap is one forward state transition, not a permanent dual-read
path. Established sessions may finish on their already-derived symmetric keys.

## Revocation

Short certificate lifetime is the primary revocation mechanism. Refusing the
next renewal bounds how long a compromised leaf remains usable without making
every connection depend on an online status service.

Emergency revocation needs a separate signed, monotonically versioned fact. It
must identify the exact certificate or subject key, not only the principal
name, because denying a name also denies a legitimate replacement key. The
fact is published by the lifecycle authority and consumed locally by every
verifier. A responder must fail closed when it has learned that a presented
certificate is revoked.

The revocation transport may use the governed namespace, but revocation
semantics do not belong in coordinator. r9p should accept a verified local
revocation view through its authentication configuration; it should not dial
an issuer during every handshake. Local subscribers update that view from a
blocking namespace feed and a durable catch-up cursor.

No revocation format or subscriber is implemented yet. It becomes required
before certificates are described as immediately revocable.

## Online issuance

An online issuer is justified by short lifetimes or renewal churn, not by the
number of hosts. It has this shape:

- the offline root remains offline;
- a constrained intermediate signs short-lived leaves;
- the issuer authenticates and authorizes every request;
- the intermediate key is exposed only to the signing boundary, preferably as
  a scoped operation rather than raw material; and
- issuance, refusal, and revocation events are auditable namespace facts.

The current certificate verifier trusts direct root signatures. Introducing an
intermediate therefore first requires an explicit chain format and validation
rules. Do not place the root key online or treat an intermediate public key as
an undeclared permanent root merely to avoid that work.

The issuer is not coordinator and is not the local auth agent. coordinator may
admit the request path. A credential authority may protect the intermediate
key behind a generic signing capability. The issuer still owns the
certificate-specific policy and canonical request validation.

## Auth agent relationship

The future local auth agent in [`auth-agent.md`](auth-agent.md) is the natural
host-side consumer of this lifecycle once unattended renewal exists. It can
keep the long-term X25519 key out of service processes, request a renewal,
atomically expose the current public certificate, and maintain the latest
verified revocation view.

That is an activation trigger, not a reason to build the agent early. The
operator-driven phase can install public material through Nix while programs
continue to use the consolidated r9p authentication library.

## Root rotation

Root rotation is an explicit fleet cutover:

1. Generate the new root offline.
1. Publish the new public trust anchor while the old root still signs the
   currently active leaves.
1. Issue and install leaves or an intermediate under the new root.
1. Prove new sessions across every required host and service boundary.
1. Remove the old trust anchor and destroy or archive the old private root
   according to operator policy.

The temporary two-root interval exists only to carry live state safely through
one forward transition. It is removed in the same lifecycle operation and must
not become a standing compatibility path.

## Implementation order

The next lifecycle work, when triggered, proceeds in this order:

1. Move all issued-principal metadata and lifetime classes into one declarative
   inventory and derive expiry health deadlines from it.
1. Define the certificate-chain and intermediate constraints without changing
   the completed Noise XX identity binding.
1. Define an idempotent issuance request and auditable result contract.
1. Build the constrained issuer and prove that caller-selected names, groups,
   and keys cannot escape its policy.
1. Add event-driven renewal and atomic installation through the local custody
   boundary.
1. Add a signed revocation snapshot and fail-closed verifier update.
1. Shorten leaf lifetimes only after renewal and revocation visibility are
   proven end to end.

At every step, a new mechanism must delete an operator or private-key handling
path. No lifecycle service is justified merely by duplicating the existing
offline commands behind a network endpoint.
