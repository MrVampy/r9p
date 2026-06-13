# r9p

`r9p` is the reusable Rust 9P library for substrate-shaped systems. It owns
9P2000 wire types, encoding/decoding, fid/tag/session mechanics, and generic
client/server protocol state. It does not own any particular filesystem,
editor, Vault, FUSE, socket, async runtime, or transport policy.

Current surfaces and consumers:

- `r9p`, a plan9port `9p`-shaped client CLI for one-shot reads, writes,
  stats, listings, creates, removes, console-style interaction, and stable
  tab/hex machine-readable output.
- `r9p mount`, the Linux FUSE-to-9P bridge.
- `r9p serve`, a read-only local filesystem-backed 9P server.
- `r9p export`, `serve` plus a machine-readable `r9p-export.v1` descriptor.
- `r9p export git`, a Git-source facade that prepares a clean detached
  worktree, writes a bundle inside it, and then enters `r9p export`.
- Racme serves an Acme-compatible 9P namespace through `r9p`.
- Vault consumes `r9p` for its runtime listener, one-shot client operations,
  local FUSE mounts, and peer export descriptors.

The architectural boundary is deliberately small:

- `r9p` speaks 9P.
- Backends decide what paths mean.
- Clients decide what they consume.
- Runtime adapters own sockets, threads, async tasks, BEAM ports, TLS, and
  FUSE.

## Scope

`r9p` incorporates both sides of the protocol:

- The server core owns session state, fid binding, request admission, and a
  backend-neutral `FileTree` trait.
- The client core owns operation construction, tag/fid allocation, and response
  admission.

The crate keeps full transport loops and operator tools layered over the
reusable client/server core.

## CLI

The operator-facing client tracks the shape of plan9port's `9p` command:

```bash
r9p [-n] [-a address] [-A aname] [-u uname] [-m msize] version [service]
r9p [-n] [-a address] [-A aname] [-u uname] [-m msize] attach [service]
r9p [-n] [-a address] [-A aname] [-u uname] [-m msize] read path
r9p [-n] [-a address] [-A aname] [-u uname] [-m msize] readfd path
r9p [-n] [-a address] [-A aname] [-u uname] [-m msize] write [-l] path
r9p [-n] [-a address] [-A aname] [-u uname] [-m msize] write-at path offset
r9p [-n] [-a address] [-A aname] [-u uname] [-m msize] writefd path
r9p [-n] [-a address] [-A aname] [-u uname] [-m msize] stat path
r9p [-n] [-a address] [-A aname] [-u uname] [-m msize] rdwr path
r9p [-n] [-a address] [-A aname] [-u uname] [-m msize] ls [-ldnt] path...
r9p [-n] [-a address] [-A aname] [-u uname] [-m msize] rm path...
r9p [-n] [-a address] [-A aname] [-u uname] [-m msize] create path...
r9p [-n] [-a address] [-A aname] [-u uname] [-m msize] mkdir path...
r9p [-n] [-a address] [-A aname] [-u uname] [-m msize] con [-r] path
r9p mount [--uname uname] [--aname aname] [--attr-timeout seconds] [--entry-timeout seconds] [--request-timeout seconds] [--lookup-timeout seconds] [--read-timeout seconds] [--write-timeout seconds] [--mutation-timeout seconds] [--control-timeout seconds] [--interrupt-timeout seconds] [--max-workers count] [--max-background count] [--congestion-threshold count] [--diagnostics-file path] [--diagnostics-capacity count] endpoint mountpoint
r9p serve [--bind address] root
r9p export [--bind address] [--descriptor machine] [--descriptor-file path] [--auth boundary] [--descriptor-field key=value] root
r9p export git [--repo path] [--rev rev] [--worktree path] [--bundle-path path] [--bundle-namespace-path path] [--bind address] [--max-fids count] [--descriptor-file path] [--auth boundary]
r9p export git ensure|status|stop --unit name [--descriptor-file path]
```

`-a` accepts `host:port`, `tcp!host!port`, bare hosts defaulting to port 564,
and `unix!/path/to/socket`. Without `-a`, paths use the plan9port namespace
shape: `service/subpath` connects to `$NAMESPACE/service` and walks `subpath`.
`-n` and `-D` are accepted for plan9port command-line compatibility; `r9p`
always uses the noauth attach path today.

The CLI is a blocking client facade over the reusable library. It is not the
boundary of the library itself.

`r9p export git` is a convenience facade over `r9p export`, not a separate
server. It resolves a commit, prepares or refreshes a clean detached worktree,
creates a Git bundle inside that worktree, and emits the normal
`r9p-export.v1` descriptor with a `git_bundle_path` extension field. The
command stays backend-neutral at the r9p layer: it serves bytes and descriptor
metadata over 9P; consumers decide how to validate Git provenance or admit a
candidate.

The lifecycle form supervises that Git export through a user systemd unit.
`ensure` and `status` print the descriptor document on stdout by default.
`--descriptor-file` is an optional extra sink when a caller also wants a
durable descriptor copy. Lifecycle supervision records the resolved commit in
the unit command, so a symbolic revision such as `HEAD` moving makes the next
`ensure` refresh the export instead of describing stale served bytes.

`r9p mount` runs a bounded worker pool rather than spawning one OS thread per
FUSE request. The defaults follow the conservative libfuse/Linux shape:
`--max-workers 10`, `--max-background 12`, and a derived congestion threshold
of 75 percent. These knobs are per mount and exist to let the kernel and the
mount client apply backpressure during broad walks or slow peer operations
instead of turning a recursive filesystem operation into an unbounded thread
or memory spike.

`r9p mount` also bounds backing 9P calls and propagates cancellation: timed-out
9P calls send `Tflush`, and Linux `FUSE_INTERRUPT` requests flush the active 9P
tag for the interrupted kernel request. `--request-timeout` remains the default
for all 9P operations, while `--lookup-timeout`, `--read-timeout`,
`--write-timeout`, `--mutation-timeout`, `--control-timeout`, and
`--interrupt-timeout` let a mount tune the expensive paths independently.
`--diagnostics-file` records JSONL operation diagnostics with opcode, unique,
nodeid, errno, and message fields. Namespace-control writes explicitly
invalidate affected FUSE inode and dentry cache entries.

`--machine` keeps the same connection flags but emits tab-separated records
with byte fields hex-encoded. It is intended for typed wrappers that need a
stable one-shot client surface without parsing the human plan9port-style output:

```bash
r9p --machine [-a address] [-A aname] [-u uname] [-m msize] version
r9p --machine [-a address] [-A aname] [-u uname] [-m msize] attach
r9p --machine [-a address] [-A aname] [-u uname] [-m msize] stat path
r9p --machine [-a address] [-A aname] [-u uname] [-m msize] list path
r9p --machine [-a address] [-A aname] [-u uname] [-m msize] read path
r9p --machine [-a address] [-A aname] [-u uname] [-m msize] readfd path
r9p --machine [-a address] [-A aname] [-u uname] [-m msize] read-to path local-path
r9p --machine [-a address] [-A aname] [-u uname] [-m msize] write path offset payload-hex
r9p --machine [-a address] [-A aname] [-u uname] [-m msize] write-at path offset
r9p --machine [-a address] [-A aname] [-u uname] [-m msize] writefd path
r9p --machine [-a address] [-A aname] [-u uname] [-m msize] write-from path offset local-path
r9p --machine [-a address] [-A aname] [-u uname] [-m msize] script script-path
r9p --machine [-A aname] [-u uname] [-m msize] script service script-path
r9p --machine [-a address] [-A aname] [-u uname] [-m msize] create path perm mode
r9p --machine [-a address] [-A aname] [-u uname] [-m msize] remove path
```

The small-payload `read` and `write` machine commands preserve the tab/hex
record format. Streaming machine commands avoid argv-sized or captured hex
payloads: `readfd` writes raw bytes to stdout, `read-to` writes raw bytes to a
local file and prints `read<TAB>count`, `writefd` reads stdin with truncating
plan9port `writefd` semantics, `write-at` reads stdin at an explicit remote
offset, and `write-from` streams a local file to an explicit remote offset.

`script` runs a tab-separated operation file over one connection and attach.
With `-a`, the command is `script script-path`; without `-a`, the command is
`script service script-path` and connects through `$NAMESPACE/service`. Blank
lines and `#` comments are ignored. Supported operations are:

```text
write-hex<TAB>remote-path<TAB>offset<TAB>payload-hex
write-from<TAB>remote-path<TAB>offset<TAB>local-path
read-to<TAB>remote-path<TAB>local-path
read-hex<TAB>remote-path<TAB>offset<TAB>count
fresh-stat-error<TAB>remote-path
```

Each completed operation prints an indexed record:
`ok<TAB>line<TAB>write<TAB>count`, `ok<TAB>line<TAB>read<TAB>count`, or
`ok<TAB>line<TAB>read-hex<TAB>count<TAB>payload-hex`. `fresh-stat-error`
opens a separate fresh attach to the same target and succeeds only if statting
the path fails, which lets wrappers prove session-private paths are not visible
outside the still-open script session. The line number is the source line in
the script file, so wrapper errors can point back to the exact operation while
preserving one 9P session for session-private state.

## Development

```bash
cargo run --bin r9p -- -a 127.0.0.1:9564 ls /
cargo test
cargo test -p cli --test fuse_mount -- --ignored
cargo clippy -- -D warnings
nix flake check
```

See [`AGENTS.md`](AGENTS.md) and [`docs/source-map.md`](docs/source-map.md)
before making protocol, compatibility, or architecture changes.
