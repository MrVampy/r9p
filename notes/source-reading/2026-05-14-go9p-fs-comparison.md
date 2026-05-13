# go9p (knusbaum) and lib9p (conclusiveeng) vs r9p — trait surface and abstraction-layer comparison

Date: 2026-05-14

## Sources checked

- `refs/knusbaum-go9p/fs/filesystem.go` — FSNode/File/Dir/ModDir interfaces, FS struct, hooks.
- `refs/knusbaum-go9p/fs/streams.go`, `fs/stream_file.go` — Stream/BiDiStream traits, StreamFile, BiDiStreamFile.
- `refs/knusbaum-go9p/fs/static.go`, `fs/dynamic.go` — built-in concrete file types.
- `refs/knusbaum-go9p/proto/fcall.go` — wire constants + FCall interface, ParseCall entry point.
- `refs/lib9p/lib9p.h`, `request.c`, `connection.c`, `genacl.c`, `threadpool.c` — multi-variant (9P2000 / .u / .L) C server, BSD-licensed, FreeBSD bhyve backend.
- `r9p/src/server/{mod,types,handlers,session,validation,config}.rs` — current trait surface and dispatch.
- `r9p/src/{codec,message,qid,fid,flush,stat}.rs` — current wire and protocol primitives.

Caps: skimmed-not-deeply-read on `lib9p/backend/fs.c` (3061 LOC), `knusbaum-go9p/union/union.go` (778 LOC), `knusbaum-go9p/client/client.go` (803 LOC). Findings about those are surface-level.

## Project shapes

| Project | LOC | Style | Variants | Notable |
|---|---|---|---|---|
| r9p | 3896 (lib only) | Sans-IO-ish, single tree-as-service trait | 9P2000 | blocking + multiplex transports, clean session/flush split |
| knusbaum-go9p | 7220 | Per-node interfaces; `fs/` is a layered abstraction above `proto/` | 9P2000 | StreamFile, UnionFS, factotum auth, multiple concrete file types |
| lib9p | 11596 | Multi-variant dispatch, C library, BSD-licensed | 9P2000, .u, .L | genacl.c (permission projection layer), threadpool.c (request scheduling), post-CVE bounds checks |

## Finding 1 — trait surface: flat-tree vs per-node

**r9p** (`src/server/mod.rs:21-54`): a single `FileTree` trait with all 9P operations as methods. Each method takes the `fid` and `qid` it operates on. Implementer owns the "tree" as one big object; dispatch by `qid`. Default impls return `EPERM` for create/write/clunk/remove/wstat so a read-only tree only implements `attach`/`walk`/`open`/`read`/`stat`.

**knusbaum-go9p** (`fs/filesystem.go:38-78`): a hierarchy of small interfaces.
- `FSNode` — Stat/WriteStat/SetParent/Parent (everything that has a place in the tree).
- `File: FSNode` — Open/Read/Write/Close per `fid`.
- `Dir: FSNode` — Children() returning `map[string]FSNode`.
- `ModDir: Dir` — AddChild/DeleteChild.

The framework walks the tree node-by-node; each node implements only the interfaces it supports.

**Trade-off:** r9p's design is closer to plan9port's `xfid` pattern — one dispatch table, walk by qid, server holds real state. go9p's design is more polymorphic: users compose filesystems from heterogeneous node types without writing flat dispatch.

For r9p's consumers so far (Racme's acme namespace, vault) the flat-tree is a fine fit because both consumers have a small, fixed set of file kinds and their "real" state lives elsewhere. For arbitrary third-party 9P servers (the substrate's eventual end-game), per-node ergonomics matter more.

**Verdict:** r9p's flat trait works for the current consumers. A higher-level `fs::` module on top — node trait + framework that drives `FileTree` underneath — is a future-extraction candidate, not a now-refactor.

## Finding 2 — concrete file-type helpers

**knusbaum-go9p** ships built-in concrete types so users don't write boilerplate for common cases:
- `StaticFile` / `StaticDir` (`fs/static.go`) — fixed bytes, read-only.
- `DynamicDir` (`fs/dynamic.go`) — child set computed by a callback.
- `StreamFile` / `BiDiStreamFile` (`fs/stream_file.go`) — per-fid reader on a stream; `Stat.Length` comes from the stream's `length()` method.
- `WrappedFile` — middleware pattern for adding behavior to an existing File.
- `UnionFS` (`union/union.go`) — multiple FS layered, plan9-style bind.

**r9p** has none of these. Consumers (Racme's acme namespace) implement them inside their own `FileTree` impl.

**Verdict:** the StreamFile pattern in particular is a real gap. Acme's `event` file is one-way stream from editor to consumer; `body` and `data` are mutable variable-length. Both could be expressed with go9p's `StreamFile`/`Stream` trait pair more concisely than the current Racme implementation. Worth lifting the pattern (per-fid reader registered on open, removed on clunk, dynamic length on stat) — even if the public API stays the flat `FileTree`.

## Finding 3 — hook pattern for filesystem-time decisions

**knusbaum-go9p** (`fs/filesystem.go:200-318`): the `FS` struct exposes optional hook functions:
- `CreateFile(fs, parent, user, name, perm, mode) -> (File, error)`
- `CreateDir(fs, parent, user, name, perm, mode) -> (Dir, error)`
- `WalkFail(fs, parent, name) -> (FSNode, error)` — synthesize a node on demand when a walk hits a path that doesn't exist (used for "lazy" file creation).
- `RemoveFile(fs, node) -> error`
- `authFunc(stream) -> (string, error)` — factotum or plain auth.

Configured via Options pattern: `NewFS(rootUser, rootGroup, perms, WithCreateFile(...), WithAuth(...))`.

**r9p** inlines these in `FileTree` with default `EPERM` impls (`server/mod.rs:28-53`). No separate auth hook (no Tauth/Rauth path observed).

**Verdict:** the hook pattern is a Go-ism that doesn't directly translate (Rust would use trait methods + builder). The interesting gap is **auth**: r9p doesn't expose a Tauth handler. plan9port servers usually rely on factotum; vault may eventually need a different auth model. Worth a focused look at how `lib9p` handles Tauth/afid (see `lib9p/request.c::np_dispatch_tauth`).

## Finding 4 — wire codec

**knusbaum-go9p/proto/fcall.go:18-50**: standard T/R constants 100-127 (same as plan9port). Each message implements `FCall` interface. `ParseCall(r io.Reader) (FCall, error)` is the single entry point.

**r9p/src/codec.rs**: 455 LOC, similar structure. Has `encode`/`decode` for each message type, plus `len_prefix` framing. No `FCall`-style trait yet — messages are an enum.

**Verdict:** r9p's enum-based dispatch is actually nicer for Rust than a trait would be. Don't change this.

## Finding 5 — flush / cancel handling

**r9p/src/flush.rs:1-124**: `FlushOutcome` enum + `RequestKey`-indexed pending-request map. Tflush triggers either `Cancel`, `AlreadyCompleted`, or `Unknown`. Integrated with server via `ServerEvent::Flush { reply, outcome }`.

**knusbaum-go9p/server.go**: flush handling lives in the dispatcher; tags are removed from an in-flight map and the original request's response is suppressed if it arrives.

**Verdict:** r9p's explicit `FlushOutcome` enum is cleaner than go9p's implicit handling. Don't change this; it's a strength.

## Finding 6 — multi-variant support (lib9p)

`lib9p/lib9p.h` declares its connection struct with a `lc_version` field that gates which message handlers run. `lib9p/request.c` dispatches on that version: 9P2000 / .u / .L share most operations but diverge on auth, stat encoding (.u adds extension fields), and the .L Linux-specific operations (Tlcreate, Treaddir, Tgetattr, Tsetattr, etc.).

**For r9p:** if we ever want a Linux kernel `mount -t 9p` to mount a Racme namespace directly (no userspace bridge), we'd need .L wire support. The cheap insurance is: add a `Variant` enum to `codec.rs` now, gate variant-specific ops via `Variant::supports(message_kind)`. Don't implement .L semantics. Just don't paint ourselves into a 9P2000-only corner at the codec layer.

This is the smallest preemptive move with the largest future optionality.

## Finding 7 — permission projection (lib9p genacl)

`lib9p/genacl.c` (720 LOC) is a separate "generic ACL" layer that projects 9P permission bits onto the host's permission model. Used because `lib9p` serves real Linux filesystems via bhyve and has to bridge plan9 mode bits to POSIX ACLs.

**For r9p:** not directly applicable — r9p's consumers (Racme, vault) serve namespaces whose permissions are projections of higher-level concepts (capability tokens, vault directives). But the *separation* — protocol layer doesn't know about permissions, projection layer translates — is worth noting. r9p currently does no permission checking; consumers enforce. That's fine and matches plan9's "server decides" model.

## Verdicts and non-actions

**Worth lifting into r9p, in priority order:**

1. **Stream-file pattern** (from `knusbaum-go9p/fs/stream_file.go`). Per-fid reader registered on open, removed on clunk, dynamic `Stat.Length`. Either as a helper on top of `FileTree` or as a new `fs::` module. *Highest value because it directly simplifies Racme's acme namespace code.*

2. **Wire-codec variant gate** (from `lib9p`). Add `Variant` enum (9P2000 today, .u/.L deferred); reject unknown ops with a "not supported in this variant" error rather than hard-coding 9P2000-only. *Cheap; preserves future optionality.*

3. **Tauth/afid path** (from `lib9p/request.c` and `knusbaum-go9p/fs/filesystem.go`). r9p doesn't have a Tauth handler; the trait surface assumes no-auth attach. Add at least a `FileTree::auth(afid, uname, aname) -> Result<Qid>` method (default: `EPERM`) and the corresponding dispatcher entry. Don't implement factotum — just don't preclude it.

**Worth keeping in r9p exactly as-is:**

- Enum-based codec dispatch (cleaner than `FCall`-trait pattern in Rust).
- `FlushOutcome` enum (cleaner than go9p's implicit dispatcher map).
- Flat `FileTree` trait for current consumers (per-node FSNode hierarchy is a follow-up, not a refactor).
- No built-in permission projection (consumers enforce at the right layer).

**Not action items:**

- Don't add `UnionFS` — too speculative; the substrate may want overlay semantics eventually but not at the r9p layer.
- Don't port go9p's hook-Options pattern; Rust idiom is trait methods + builder, not registered function pointers.
- Don't lift go9p's auth backends (factotum, SASL) — too specific to plan9-era ecosystem; vault may need something else.

## Open questions for follow-up

1. Does the StreamFile pattern fit r9p's current FileTree shape, or does it require a parallel API? (Read `r9p/src/server/handlers.rs` open/read/clunk paths to find out.)
2. What does Racme's `racme-acme` event file actually do today, and would a StreamFile abstraction replace its implementation cleanly? (Cross-check after the M3 close-out.)
3. Is there an existing 9P2000.L user-space client in production that we'd want r9p compatible with, or is this purely defensive? (If purely defensive, the variant gate is enough; no .L implementation needed.)
