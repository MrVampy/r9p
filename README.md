# r9p

`r9p` is the reusable Rust 9P library. It owns
9P2000 wire types, encoding/decoding, fid/tag/session mechanics, and generic
client/server protocol state. It does not own any particular filesystem,
editor, coordinator, FUSE, socket, async runtime, or transport policy.

Current surfaces and consumers:

- `r9p`, a plan9port `9p`-shaped client CLI for one-shot reads, writes,
  stats, listings, creates, removes, console-style interaction, and stable
  tab/hex machine-readable output.
- `r9p mount`, the Linux FUSE-to-9P bridge.
- `r9p serve`, a local filesystem-backed 9P server that is read-only by
  default and explicitly writable with `--writable`.
- `r9p export`, `serve` plus a machine-readable `r9p-export.v1` descriptor.
- `r9p reverse-broker` and `r9p reverse-export`, an authenticated outbound
  publication posture for a filesystem owner that cannot accept inbound
  connections.
- `9P2000.R` namespace referrals, which let one caller-local client compose
  admitted direct service sessions behind ordinary namespace paths.
- `r9p auth-keygen`, key creation for authenticated remote 9P sessions.
- `r9p cert`, an offline signer that binds a name and groups to a session key
  so relying parties learn a principal's name instead of asserting it.
- Racme serves an Acme-compatible 9P namespace through `r9p`.
- coordinator consumes `r9p` for its listener, one-shot client operations,
  local FUSE mounts, and admitted service referrals.

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

Live files use retained fids, blocking tagged reads, and `Tflush`
cancellation. Whether one fid may carry one or several pending reads depends on
whether the file uses an implicit per-fid cursor or independent positional
cursors. See
[`docs/guides/event-driven-9p.md`](docs/guides/event-driven-9p.md) for the complete pattern.

The server core also owns the invariant part of `Twstat`. It rejects attempts
to change `type`, `dev`, `qid`, `atime`, `uid`, `muid`, or a file's type bits
before dispatching to a backend. Backends own permissions and storage-specific
fields, but a successful backend `wstat` must apply every requested change or
none. `Stat::null_wstat()` is the single constructor for the protocol's
explicit "don't touch" values.

## Protocol Variants

Plain clients and servers negotiate `9P2000`. Extension-aware peers negotiate
`9P2000.R`, the r9p dialect. It adds only these semantics:

- `QTSYMLINK` and `DMSYMLINK` identify symbolic links.
- Opening and reading a symbolic link returns its target bytes.
- `Treferrals` and `Rreferrals` return finite admitted direct targets for
  mounted logical prefixes.
- The caller-local session client establishes and reuses those direct sessions
  while callers continue to use ordinary walks, fids, reads, writes, and RPCs.
- A peer that negotiates plain `9P2000` must not receive symlink qids or stat
  bits.

This is not 9P2000.u or 9P2000.L. r9p does not claim their stat fields, numeric
identity fields, or error semantics. An extension-capable client accepts a
server downgrade to plain `9P2000`; symlink metadata and referrals after such a
downgrade are protocol errors. P9any and Noise authenticate the byte stream
before version negotiation and are not part of the dialect. `r9p export`
advertises the exact protocol in its descriptor.

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
r9p [-n] [-a address] [-A aname] [-u uname] [-m msize] con [--resume] [-r] path
r9p [-n] [-a address] [-A aname] [-u uname] [-m msize] stream path
r9p mount [--source namespace-path] [--uname uname] [--aname aname] [--attr-timeout seconds] [--entry-timeout seconds] [--request-timeout seconds] [--lookup-timeout seconds] [--read-timeout seconds] [--write-timeout seconds] [--mutation-timeout seconds] [--control-timeout seconds] [--interrupt-timeout seconds] [--max-workers count] [--max-background count] [--congestion-threshold count] [--diagnostics-file path] [--diagnostics-capacity count] endpoint mountpoint
r9p mount ensure|status|stop --mountpoint path [--unit name --unit-scope user|system] [--status-file path] [--expect-endpoint endpoint] [--expect-change-feed path] [--expect-status-file path] [--attempts count] [-- mount args...]
r9p serve [--bind address] [--max-fids count] [--writable] root
r9p export [--bind address] [--max-fids count] [--writable] [--descriptor machine] [--descriptor-file path] [--auth-config path] [--descriptor-field key=value] root
r9p reverse-broker --reverse-bind address [--proxy-bind address] [--proxy-exposure local|authenticated-network] --principal name --auth-config path [--pool count]
r9p reverse-export --connect address --principal name --auth-config path [--pool count] [--reconnect-min-delay seconds] [--reconnect-max-delay seconds] [--writable] root
r9p session-proxy --bind loopback-address|unix!/path --connect address --principal name --auth-config path [--max-sessions count]
r9p auth-keygen --private path --public path [--private-access owner-only|owner-group-read]
r9p cert root --private path --public path
r9p cert sign --root-private path --name name (--key hex | --key-file path) [--group name]... (--days n | --not-after seconds) [--not-before seconds] [--out path]
r9p cert print --path path [--at seconds]
r9p cert verify --path path (--root hex | --root-file path) [--at seconds]
```

`-a` accepts `host:port`, `tcp!host!port`, bare hosts defaulting to port 564,
and `unix!/path/to/socket`. Without `-a`, paths use the plan9port namespace
shape: `service/subpath` connects to `$NAMESPACE/service` and walks `subpath`.
`--auth-domain` names the responder a dial requires; referrals supply their own
from the authority boundary, so one credential serves them all. `-n` and
`-D` are accepted for plan9port command-line compatibility; `r9p` uses
`NOFID` attach because remote authentication is completed before the 9P
version and attach exchange.

The CLI is a blocking client facade over the reusable library. It is not the
boundary of the library itself.

`con --resume` is an explicit replay contract for a stream file whose read and
write offsets are durable application cursors. After a definitive transport
failure, r9p reattaches, reopens the same path, and repeats the operation at the
same offset. It must not be used with an ordinary mutable file whose repeated
write would apply the effect twice.

`stream` is the machine-facing full-duplex stdio adapter. It retains a
read-only fid and a write-only fid on one multiplexed 9P connection, copies
bytes without carriage-return filtering or an escape character, and flushes
each received chunk to stdout. Stdin EOF clunks the write fid so the service
can close its input side and finish any remaining output. The command
deliberately provides no automatic replay: a protocol request may have caused
a non-idempotent effect before transport failure made delivery uncertain. This
makes `stream` suitable as an opaque carrier for protocols such as MCP without
teaching r9p their message semantics.

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

### Certificates

`r9p cert` puts a principal name and its stable role groups in signed material.
An offline Ed25519 root signs
over a session key's *public* half, so the private half never leaves the host
that generated it - `auth-keygen` still creates it, and the certificate is
issued afterwards:

```bash
r9p cert root --private /var/lib/r9p/cert/root --public /var/lib/r9p/cert/root.pub
r9p cert sign \
  --root-private /var/lib/r9p/cert/root \
  --name tuxedo \
  --key-file /var/lib/r9p/auth/public \
  --group operator \
  --days 730 \
  --out tuxedo.crt
r9p cert verify --path tuxedo.crt --root-file /var/lib/r9p/cert/root.pub
```

Session keys are X25519 Noise statics and cannot sign, so the root is Ed25519
and signs over them. The signature covers a canonical length-framed encoding
built from the parsed fields rather than the file text, so reformatting cannot
change what was signed and no field boundary can be shifted. Validity is whole
seconds since the Unix epoch, which keeps a calendar library out of the trust
path; `r9p cert print` reports `expires_in_seconds`, the form a threshold alert
wants. Certificates are public material and are written mode `0644`; signing
refuses to overwrite an existing one.

A client presents its certificate by naming it in the client config; a server
accepts certificates by naming the roots it trusts:

```text
# client
certificate /var/lib/r9p/cert/tuxedo.crt

# server
root 217a98d48bc5c82dbbab66272009ff7ced583321a4b9e6bae6f5db04ae1ce183
```

The presented certificate must have been issued for the key that completed the
handshake, so a certificate - which is public - cannot be replayed by anyone
else holding a copy. The certified principal becomes the remote transport
subject `r9p-cert:<name>`, and the certificate groups are available to coarse
role authorization. Admission policy names the certified principal rather
than its current public key, so key rotation does not rewrite policy.

The flake exports `nixosModules.session-auth` for boot-time provisioning and
verification without host-specific scripts. After importing the module, declare
each key pair and its owner:

```nix
services.r9p-session-auth.keys.namespace = {
  privateKeyFile = "/var/lib/r9p-session-auth/namespace.key";
  publicKeyFile = "/var/lib/r9p-session-auth/namespace.key.pub";
  user = "namespace-runtime";
  group = "namespace-runtime";
};
```

The default `0700` directory and `0600` private key keep one identity private
to one Unix user. When two cooperating services deliberately share one
certified identity, set a dedicated group, `directoryMode = "0750"`, and
`privateKeyAccess = "owner-group-read"`. Add the matching local access policy
to each session-auth config that consumes the key. This grants only that group
access and does not require either service to retain discretionary-access
override capabilities:

```text
private-key-access owner-group-read
```

A server config presents its own certificate and names the roots it trusts:

```text
format r9p-session-auth.v1
role server
domain namespace
private-key /var/lib/r9p/auth/private
certificate /var/lib/r9p/auth/namespace.crt
root 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
```

A client config names no service and pins no key. It carries the identity it
presents and the roots it trusts, so the same file reaches every service:

```text
format r9p-session-auth.v1
role client
private-key /var/lib/r9p/auth/private
certificate /var/lib/r9p/auth/codex.crt
root 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
```

Relative private-key paths resolve from the config file directory. A protected
client call and export then use, with `--auth-domain` naming the responder the
client requires:

```bash
r9p --auth-config client.conf --auth-domain namespace \
    -a 192.168.0.30:9564 -u codex ls /
r9p export --bind 192.168.0.30:9564 --auth-config server.conf /srv/export
```

The peers negotiate `noise-xx@<domain>` with p9any and carry 9P over
ChaCha20-Poly1305 records with BLAKE2s. Both X25519 static keys travel inside
the handshake, encrypted, and each is authenticated by a certificate from a
root the other side trusts, so neither side configures the other's key. The
dialling side names the service it expects; a responder holding a valid
certificate for a different name cannot answer in its place. Each session
creates its own ephemeral handshake state; there is no per-session operator
setup. The provider follows p9any's extensible negotiation shape but does not
claim dp9ik or unmodified factotum interoperability.

The completed transport is separate from certificate lifecycle. See
[`docs/design/certificate-lifecycle.md`](docs/design/certificate-lifecycle.md)
for the current offline-signing phase and the gates for future renewal,
revocation, and a local key-custody agent.

`authenticate_server` proves the Noise static key and the certified name bound
to it. It does not admit the caller: mapping a subject to a namespace principal
remains application policy. `TransportIdentity` exposes the signed name as
`r9p-cert:<name>`. On Unix sockets it derives
`unix-peer:uid:<uid>` from `SO_PEERCRED`, plus `unix-peer:same-user` when the
peer runs under the listener's effective UID. Mapping those subjects to
namespace principals remains application policy.

### Reverse-connect 9P

`reverse-export` changes connection placement, not the 9P protocol or the
exported tree. The filesystem-owning host authenticates outward to a
`reverse-broker` and serves an ordinary 9P session on every connected stream.
The broker exposes a local proxy endpoint by default and copies bytes between
one client and one authenticated reverse stream. It does not parse 9P, resolve
service names, admit capabilities, or own the exported files.

The pool and all listener work are bounded. Failed exporter connections use a
capped exponential retry delay with deterministic worker phasing; successful
sessions replenish the pool immediately. Before assigning a queued stream, the
broker discards peers whose TCP close is already observable. Status snapshots
report pool, handshake, bridge, rejection, and failure counters to embedding
applications.

Applications that need an end-service identity boundary over the placement
link use `ReverseExport::start_authenticated` or
`ReverseExport::start_authenticated_handler`. The broker-to-exporter
authentication then proves the placement peer, while a second p9any/Noise
session carried transparently through that stream proves the final service
client. The broker still does not parse 9P or learn application policy.
Applications whose per-connection handler owns group-based authorization use
`ReverseExport::start_authenticated_handler_with_peer`; its factory receives
the already verified principal, public key, and certificate groups.

Reverse transport sockets use bounded TCP keepalive in addition to disabling
Nagle. This makes an application-idle pool detect a hard peer outage and enter
the existing reconnect loop instead of retaining apparently established
streams for the host kernel's long default timeout.

This is a runtime adapter, not a registration system. A service may publish the
broker's ordinary endpoint using its existing registry lifecycle, but service
naming, leases, capability admission, and direct-versus-relay choice remain the
responsibility of the governing namespace. Local proxy exposure remains the
default. `--proxy-exposure authenticated-network` accepts only a concrete
non-loopback TCP endpoint and is valid only when the reverse exporter uses
`ReverseExport::start_authenticated` or
`ReverseExport::start_authenticated_handler`, so the final service
authenticates every client through the placement stream. Without that
end-to-end boundary, network exposure would create an ambient proxy.

`session-proxy` is the forward counterpart for a host-local consumer that must
use an authenticated remote session without receiving the transport private
key. It accepts only a loopback TCP or local Unix endpoint. Each bounded local
connection receives a fresh authenticated upstream session under the one fixed
principal selected by host configuration. It does not resolve service names,
select namespace paths, inspect 9P, or provide remote access.

### Transparent namespace referrals

An admitted root may answer `Treferrals` with `NamespaceReferral` records. Each
record carries a mounted logical prefix, endpoint, attach identity, exported
root, portable authority boundary, generation, and finite relative validity.
These records are protocol mechanism and never appear as files in the logical
namespace.

`session::Client` asks for referrals after attaching to a `9P2000.R` root,
selects the longest matching mounted prefix, and lazily establishes the direct
service session in the caller process. Established sessions are retained and
reused; expired unconnected referrals are refreshed through the root. Ordinary
client, CLI, FUSE, front ABI, and BEAM operations continue to use namespace
paths and fids. They do not resolve a public control path or relay service bytes
through the root.

`r9p mount --source /namespace/subtree ENDPOINT MOUNTPOINT` presents the
selected ordinary namespace subtree as the local FUSE root. Referral selection,
direct connection establishment, reconnect, and path rebinding remain internal
to the r9p client. Omitting `--source` selects `/`.

Referrals carry portable authority names, never local credential paths. A
session holds one credential and reuses it across every boundary it crosses;
the referral's domain, such as `p9any:noise-xx@agents`, is the responder name
its certificate must prove. Contained boundaries such as loopback, Unix
sockets, and admitted network classes need no credential at all. Receiving
a referral is not transport authentication, and moving the client to the
service host does not prove the original caller can reach the endpoint.

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
