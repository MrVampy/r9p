---
name: r9p
description: Operate admitted 9P services and composed namespaces with the installed r9p client. Use for namespace discovery, typed file operations, same-fid RPCs, blocking reads, streams, sessions, exports, or FUSE mounts. Do not use it as a substitute for ordinary local filesystem tools.
---

# Operate a governed namespace

Use the Nix-installed `r9p` on `PATH`. Begin with `r9p --help` and the exact
subcommand help needed for the operation. Treat those current command surfaces
as authoritative instead of relying on a copied command list.

When `$NAMESPACE` is set, use logical paths such as `memory/status` or
`newsgroups/resolve/guide`. The first path element resolves through the
already-admitted local projection, and r9p follows governed referrals itself.
Do not discover or guess a Coordinator endpoint, service host, authentication
domain, principal, or credential path. Use `--bind` only when the surrounding
request explicitly supplies an admitted endpoint and authentication context.

Inspect a service's `guide`, `manifest`, `status`, and advertised next actions
before mutating it. Prefer ordinary namespace operations - read, write, create,
remove, walk, stat, or retained blocking reads - whenever they express the
effect. Use a same-fid RPC only when the service contract actually defines one.

Use `r9p list` or `--machine` when another program or agent will consume the
result. Keep independent blocking operations on independent sessions. Treat an
interrupted mutation as delivery-unknown unless its service contract provides
an operation ID or durable result path; never replay it merely because the
connection failed.

Authorization comes from the admitted namespace session, not from request
fields. Never read, print, move, or synthesize authentication material while
using this skill.
