# Plan 9 and 9P audit of r9p

Date: 2026-07-20

## Question

Which ideas from Plan 9, Inferno, current 9P implementations, and Linux FUSE
should be adopted into r9p now, and which should remain outside the protocol
core? The audit also asked whether centralizing those rules lets r9p callers
and backends become smaller and easier to maintain.

## Local sources checked

Current r9p behavior was traced through these paths:

- `crates/core/src/codec.rs`, `client.rs`, `client_support.rs`, `fid.rs`,
  `flush.rs`, `mode.rs`, `stat.rs`, and `server/`.
- `crates/core/src/blocking.rs` and `multiplex/`.
- `crates/front/src/serve.rs`, `tree.rs`, and `model.rs`.
- `crates/fs/src/lib.rs` and `unix_io.rs`.
- `crates/session/src/control/`, `crates/fuse/src/fuse/`, and the CLI serving
  and transport paths.
- `refs/vault/docs/source-map.md`, `operations/9p-endpoint.md`,
  `operations/plan9port-client.md`, and the Vault Plan 9 landscape notes.

Plan 9 lineage and protocol behavior were checked against:

- `refs/9front/sys/man/5/version`, `walk`, `open`, `read`, and `stat`.
- `refs/9front/sys/src/lib9p/srv.c`, `auth.c`, and `fid.c`.
- The corresponding `refs/9legacy/sys/man/5/` and
  `refs/9legacy/sys/src/lib9p/` paths.
- `refs/plan9port/src/lib9p/`, `refs/plan9port/man/man9/`, and the plan9port
  `9p` client sources.
- `refs/inferno-os` for namespace inheritance and sandbox lineage. Those ideas
  remain Vault/runtime concerns rather than 9P wire behavior.

Comparative implementations and bridges included local references for go9p,
rust-p9, rs9p, p92000l/recover9pl, arigato, r9-fileserver, 9pfuse, Linux FUSE,
and libfuse.

## Current upstream sources checked

- The [Linux 9P documentation](https://docs.kernel.org/6.1/filesystems/9p.html)
  for the kernel client's 9P2000, 9P2000.u, and 9P2000.L surface.
- The [gVisor LisaFS design](https://github.com/google/gvisor/blob/master/pkg/lisafs/README.md)
  and [gVisor p9 package](https://pkg.go.dev/gvisor.dev/gvisor@v0.0.0-20260506072103-91f5a5a31645/pkg/p9)
  for high-throughput local sharing lessons.
- The [diod repository](https://github.com/chaos/diod) for a current 9P2000.L
  server's recovery, cache-coherency, and confidentiality cautions.
- The [u-root ninep repository](https://github.com/u-root/ninep),
  [pfpacket rust-9p repository](https://github.com/pfpacket/rust-9p), and
  [awesome-9p index](https://github.com/henesy/awesome-9p) as implementation
  and ecosystem comparisons.

## Findings adopted

### Protocol lifecycle belongs in core

Plan 9 `lib9p` centrally checks whether a fid is opened, whether its mode permits
the requested operation, whether a walk starts from an unopened directory, and
whether directory reads continue at the previous reply boundary. r9p had an
unused boolean open field while each backend carried parts of that policy.

r9p now records the exact open mode and directory offset in `FidState`. The
server core enforces walk/open/create/read/write sequencing, valid open bits,
directory modes, auth-fid behavior, and directory offsets before dispatching a
backend request. Auth fids become open read-write fids when `Rauth` succeeds, as
in 9front `lib9p/auth.c`.

Split request completion also needs transition ownership. New fids and
state-changing operations now receive exclusive reservations. Clone walks use
shared source reservations and exclusive target reservations: unrelated clone
walks can overlap, but a clunk, open, create, remove, in-place walk, or reset
cannot invalidate their source mid-request. Flush and version reset release
those reservations, stale completions cannot rebind a fid, and a retired clunk
or remove fid cannot be reused while its backend operation is still in flight.

### Partial walks leave both fids unchanged

The Plan 9 `walk(5)` contract says that a partial `Rwalk` reports the qids for
successful elements but affects `newfid` only when every requested element was
walked. 9front `lib9p/rwalk` removes its provisional new fid on a partial walk.
r9p core, front, and filesystem backends retain that behavior and now have an
explicit regression for it. A zero-qid non-empty walk becomes `Rerror`.

### Version and frame boundaries are one contract

The server now requires `Tversion`, returns `Rversion` with `unknown` rather
than `Rerror` for an unsupported version, resets to an unnegotiated state after
an invalid negotiation, and accepts only the exact plain version or a
period-separated extension that can be downgraded to plain 9P2000.

Checked frame reads and writes now live only in `core::codec`. They distinguish
clean EOF from a truncated length prefix and enforce the negotiated `msize` in
both directions. The unbounded stream helpers were removed, and every
production and test caller that had its own four-byte prefix parser now uses
the checked boundary. Client response admission verifies the outstanding
`NOTAG` negotiation, the returned `msize`, live response tags, and open/create
`iounit` bounds. Read and write reply counts cannot exceed their requests, and
offset arithmetic fails instead of silently saturating.

### Connection and backend reset logic are shared

`serve_file_tree_connection` is now the synchronous adapter over the same
connection state machine used by asynchronous fronts. The CLI filesystem
exporter and session-control server only construct a tree and configuration;
they no longer implement framing or connection loops.

The `FileTree` contract has a session reset hook. A synchronous `Server` and the
connection adapter invoke it for `Tversion`, so protocol fids and backend-local
handles cannot diverge across sessions. The local filesystem clears fids,
opened descriptors, and cached stats; the front starts a new front session;
the control tree clears fid-keyed query responses.

### Backends use shared vocabulary

Open-mode constants and predicates now have one owner in `core::mode`. Core,
front, filesystem, session, and FUSE callers consume them directly. The
historical error constant named `EEXIST` but containing "file does not exist"
was split into correct `ENOENT` and `EEXIST` constants, and extraction residue
in anonymous stat identities and authentication errors was removed.

The filesystem and front backends now focus on permissions, namespace content,
and application effects. They no longer duplicate generic fid lifecycle,
framing, error vocabulary, or connection reset rules.

### FUSE capabilities must describe implemented semantics

The bridge no longer advertises `FUSE_EXPORT_SUPPORT`: its forgotten nodeids
are intentionally retired, so it cannot promise exportfs stale-handle lookup.
It also no longer advertises `FUSE_DONT_MASK`, because the bridge does not apply
the `FuseCreateIn.umask` itself. Linux therefore remains responsible for
applying the caller's umask. Capability regressions cover both decisions.

## Findings retained without change

- Tags, generation-checked flush handling, multi-element walks, large
  negotiated messages, multiplexed clients, reconnectable sessions, namespace
  change feeds, and targeted FUSE invalidation are already strong r9p features.
- Vault governance remains above r9p. Attach identity, namespace admission,
  `/srv` policy, service meaning, and observable reads and writes continue to
  belong to Vault.
- Vault's front intentionally permits a slash-containing `Tcreate` name for
  service-registration bootstrap before the grouping directories exist. The
  generic core therefore validates walk components but does not impose a
  single-component create-name policy on every backend.
- The flat `FileTree` boundary remains appropriate for current backends. A
  per-node framework, union filesystem, or generic stream-file library should
  be added only for a concrete consumer that becomes simpler with it.

## Ideas not imported now

### 9P2000.u and 9P2000.L

Linux documents all three dialects, but adding a dialect name without its
stat, error, and operation semantics would create a false contract. r9p keeps
one explicit plain variant and clean downgrade behavior. Direct Linux v9fs
serving or a named external 9P2000.L consumer would justify a complete dialect
slice.

r9p currently uses the 9P2000.u symlink type and mode bits internally for its
filesystem/FUSE bridge while negotiating plain 9P2000. That limited extension
is useful but should be formalized as a dialect decision before claiming
portable external symlink interoperability.

### LisaFS-style local descriptors

gVisor replaced 9P for its high-throughput local host/guest path because
round-trip count, descriptor donation, batching, and multiple communicators
matter more there than adding more 9P opcodes. r9p already has concurrent tags,
multi-walk, large `msize`, `READDIRPLUS`, reconnect, and invalidation for the
Vault namespace workload. Local descriptor donation or a second transport
protocol should wait for a benchmark that identifies data-plane copying or
round trips as the actual limit.

### Transport confidentiality and cache coherence

diod's cautions reinforce an existing boundary: 9P does not itself provide
confidentiality, automatic recovery, or coherent distributed caching. r9p's
local Unix and loopback paths are suitable for the current runtime. Any
non-loopback deployment still needs an explicit authenticated and encrypted
transport boundary; it must not be implied by 9P negotiation.

## Remaining follow-ups

- Run the host-gated FUSE mount, restart, invalidation, and parallel traversal
  suite on the M7 execution host. Unit and conformance coverage cannot replace
  `/dev/fuse` behavior.
- Decide whether to formalize the internal symlink bits as 9P2000.u or as a
  documented r9p-local plain extension before exposing them to an external
  kernel client.
- Add a focused source-backed slice for immutable `Twstat` fields and atomic
  backend mutation if an untrusted writable server surface needs stronger
  generic enforcement. The core currently owns lifecycle while backends still
  own detailed wstat policy.
- Add transport authentication/encryption only alongside a named non-loopback
  threat model and deployment consumer.
- Revisit descriptor donation, batching beyond multi-walk, or 9P2000.L only
  after workload measurements show that the current namespace-oriented path is
  insufficient.

## Verification

- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --all -- --check`
- `git diff --check`
- The built client negotiated 9P2000 and statted `/` against the local 9front
  export on `127.0.0.1:564` and the local 9legacy export on
  `127.0.0.1:1564`.
- Repository scans confirmed that stream framing is confined to the core codec
  and open-mode definitions have one owner in the core mode module.
