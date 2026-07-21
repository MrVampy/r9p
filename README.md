# r9p

`r9p` is the reusable Rust 9P library. It owns
9P2000 wire types, encoding/decoding, fid/tag/session mechanics, and generic
client/server protocol state. It does not own any particular filesystem,
editor, Vault, FUSE, socket, async runtime, or transport policy.

Current surfaces and consumers:

- `r9p`, a plan9port `9p`-shaped client CLI for one-shot reads, writes,
  stats, listings, creates, removes, console-style interaction, and stable
  tab/hex machine-readable output.
- `r9p mount`, the Linux FUSE-to-9P bridge.
- `r9p serve`, a local filesystem-backed 9P server that is read-only by
  default and explicitly writable with `--writable`.
- `r9p export`, `serve` plus a machine-readable `r9p-export.v1` descriptor.
- `r9p auth-keygen`, key creation for authenticated remote 9P sessions.
- Racme serves an Acme-compatible 9P namespace through `r9p`.
- Vault consumes `r9p` for its runtime listener, one-shot client operations,
  local FUSE mounts, and peer export descriptors.

The architectural boundary is deliberately small:

- `r9p` speaks 9P.
- Backends decide what paths mean.
- Clients decide what they consume.
- `r9p-auth` owns p9any negotiation, peer authentication, and encrypted stream
  records above the protocol core.
- Runtime adapters own sockets, threads, async tasks, BEAM ports, and FUSE.

## Scope

`r9p` incorporates both sides of the protocol:

- The server core owns session state, fid binding, request admission, and a
  backend-neutral `FileTree` trait.
- The client core owns operation construction, tag/fid allocation, and response
  admission.

The crate keeps full transport loops and operator tools layered over the
reusable client/server core.

For embedded servers that need cancellable blocking work without selecting an
async runtime, `r9p::server::serve_connection` is an optional connection
facade. It drives one caller-supplied cloneable `Read + Write` stream, delegates
request meaning to a `ConnectionHandler`, and bounds live asynchronous workers
with `ServerConfig::max_async_requests`. `Tflush`, fid clunk, version reset, and
connection teardown signal cancellation; worker capacity is released only
when the handler actually exits. The application still owns listeners, socket
creation and permissions, peer admission, TLS, and process lifecycle.

Synchronous backends can use `serve_file_tree_connection`, which adapts a
`FileTree` to the same checked framing, version reset, and connection state
machine without duplicating a request loop.

The server core also owns the invariant part of `Twstat`. It rejects attempts
to change `type`, `dev`, `qid`, `atime`, `uid`, `muid`, or a file's type bits
before dispatching to a backend. Backends own permissions and storage-specific
fields, but a successful backend `wstat` must apply every requested change or
none. `Stat::null_wstat()` is the single constructor for the protocol's
explicit "don't touch" values.

## Protocol Variants

Plain clients and servers negotiate `9P2000`. The filesystem exporter and
FUSE session can additionally negotiate `9P2000.r9p-symlink`, a deliberately
narrow r9p extension. It adds only these semantics:

- `QTSYMLINK` and `DMSYMLINK` identify symbolic links.
- Opening and reading a symbolic link returns its target bytes.
- A peer that negotiates plain `9P2000` must not receive symlink qids or stat
  bits.

This is not 9P2000.u. r9p does not claim the 9P2000.u stat extension, numeric
identity fields, or error semantics. An extension-capable client accepts a
server downgrade to plain `9P2000`; symlink metadata after such a downgrade is
a protocol error. `r9p export` advertises the exact extension in its descriptor.

Blocking consumers that require finite transport calls can use
`r9p::blocking::connect_endpoint_with_timeouts` with `ConnectionTimeouts` for
TCP, `unix!`, or `unix:` endpoints. Distinct read and write timeouts are
installed before the version and attach handshake; the connect timeout applies
to each resolved TCP socket address. `Client::connect_tcp_with_timeouts` uses
the same timeout contract when a typed TCP client is useful.

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
r9p mount ensure|status|stop --mountpoint path [--unit name --unit-scope user|system] [--status-file path] [--expect-endpoint endpoint] [--expect-change-feed path] [--expect-status-file path] [--attempts count] [-- mount args...]
r9p serve [--bind address] [--max-fids count] [--writable] root
r9p export [--bind address] [--max-fids count] [--writable] [--descriptor machine] [--descriptor-file path] [--auth-config path] [--descriptor-field key=value] root
r9p auth-keygen --private path --public path
```

`-a` accepts `host:port`, `tcp!host!port`, bare hosts defaulting to port 564,
and `unix!/path/to/socket`. Without `-a`, paths use the plan9port namespace
shape: `service/subpath` connects to `$NAMESPACE/service` and walks `subpath`.
`-n` and `-D` are accepted for plan9port command-line compatibility; `r9p`
uses `NOFID` attach because remote authentication is completed before the 9P
version and attach exchange.

The CLI is a blocking client facade over the reusable library. It is not the
boundary of the library itself.

`--unit` and `--unit-scope` are a pair. `user` targets the per-user systemd
manager and `system` targets the system manager. The selected scope applies
consistently to status, ensure, and stop operations.

### Authenticated TCP sessions

Non-loopback `r9p export` endpoints require a server authentication config.
Clients opt into the same boundary with the global `--auth-config` option.
The suite generates each host key pair without external crypto tooling:

```bash
r9p auth-keygen \
  --private /var/lib/r9p/auth/private \
  --public /var/lib/r9p/auth/public
```

The command creates a mode `0600` private key and a mode `0644` public key. On
later runs it verifies that the pair still matches. If a completed private-key
write is missing only its public key, the command reconstructs the public key;
a public-only or mismatched pair fails without overwriting either file.

The flake exports `nixosModules.session-auth` for boot-time provisioning and
verification without host-specific scripts. After importing the module, declare
each key pair and its owner:

```nix
services.r9p-session-auth.keys.vault = {
  privateKeyFile = "/var/lib/r9p-session-auth/vault.key";
  publicKeyFile = "/var/lib/r9p-session-auth/vault.key.pub";
  user = "vault-runtime";
  group = "vault-runtime";
};
```

A server config maps public keys to the exact 9P usernames they may claim:

```text
format r9p-session-auth.v1
role server
domain vault
private-key /var/lib/r9p/auth/private
peer 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef codex
```

A client config pins the server public key:

```text
format r9p-session-auth.v1
role client
domain vault
private-key /var/lib/r9p/auth/private
server-key fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210
```

Relative private-key paths resolve from the config file directory. A protected
client call and export then use:

```bash
r9p --auth-config client.conf -a 192.168.0.30:9564 -u codex ls /
r9p export --bind 192.168.0.30:9564 --auth-config server.conf /srv/export
```

The peers negotiate `noise-ik@<domain>` with p9any, authenticate their pinned
X25519 static keys, and carry 9P over ChaCha20-Poly1305 records with BLAKE2s.
Each session creates its own ephemeral handshake state; there is no per-session
operator setup. The provider follows p9any's extensible negotiation shape but
does not claim dp9ik or unmodified factotum interoperability.

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
r9p --machine [-a address] [-A aname] [-u uname] [-m msize] rpc-hex path request-hex
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
`rpc` and machine `rpc-hex` use `--control-timeout` rather than the shorter
default request timeout, because a single-fid RPC may wait on external service
work before the first response read returns.

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
