# r9p Architecture

`r9p` is the reusable 9P protocol primitive. It is intentionally narrower than a filesystem, narrower than a FUSE bridge, and narrower than any one application.

## Boundary

```text
backend
  Racme Acme tree, Vault namespace adapter, exportfs-style host tree, memory fixture

r9p server core
  9P messages, qids, fids, tags, stat records, version negotiation,
  walk/open/read/write/clunk/flush lifecycle and response bounds

optional r9p connection facade
  checked frames, split admission/completion, bounded cancellable workers

transport adapter
  Unix socket, TCP, stdio, BEAM port, virtio transport

r9p client core
  9P operation builders and response admission

consumer
  Rust program, FUSE bridge, export helper, test harness, namespace service
```

The core rule is: `r9p` speaks 9P; backends decide what to serve; consumers decide what to do with the bytes; runtime adapters decide how bytes move.

The optional connection facade is layered over the same transport-neutral
`Server::admit` and `Server::complete` state machine. `serve_connection`
accepts a custom `ConnectionHandler` for asynchronous or cancellable work;
`serve_file_tree_connection` adapts an ordinary synchronous `FileTree`. Both
own checked framing, version resets, response serialization, and bounded worker
accounting for one already-created cloneable byte stream. They do not bind
sockets or decide endpoint permissions, peer identity, authentication, service
admission, TLS, or daemon lifecycle. A runtime with its own executor can
continue to use `admit` and `complete` directly.

The server core owns wire-level fid lifecycle. It records open modes and
directory offsets, rejects operations that violate 9P sequencing, and reserves
fid transitions across split request completion. Clone walks share a source
reservation so independent walks can overlap, while open, create, clunk,
remove, in-place walk, and version reset cannot race that source transition.
Backends still own namespace meaning, permissions, content, and application
effects. A `FileTree` reset clears backend session-local state when `Tversion`
starts a new session.

The core owns `Twstat` fields whose mutability is fixed by 9P2000. Immutable
fields and file-type changes are rejected before backend dispatch. The backend
then validates its complete mutable-field policy before changing storage and
must provide all-or-none completion. A backend that cannot make a requested
combination atomic rejects it; it must not apply a prefix of the request.

Variant negotiation is capability negotiation, not a label. Plain `9P2000`
never exposes symlink qids or stat bits. `9P2000.r9p-symlink` is the one narrow
r9p extension: it admits `QTSYMLINK` and `DMSYMLINK` and the existing
read-target representation used by the filesystem exporter and FUSE bridge.
It deliberately does not claim 9P2000.u. Servers configured for the extension
can downgrade a plain requester, while extension-aware clients reject symlink
metadata after a downgrade.

`r9p` provides generic client create, write, remove, read, and RPC operations,
plus language bindings that encode the transport-neutral `r9p-export.v1`
descriptor. An application that registers with a runtime owns the lifecycle
that writes that descriptor through the runtime's ordinary namespace. The
runtime owns `/srv` admission, lease interpretation, and projection. Neither
side gives `r9p` Vault-specific registration policy or a privileged runtime
control path.

The blocking TCP client has an opt-in bounded connection seam:
`Client::connect_tcp_with_timeouts` takes independent connect, read, and write
timeouts. It resolves the endpoint, uses `TcpStream::connect_timeout` for each
resolved address, and installs the read and write timeouts before 9P version
negotiation and attach. The existing `Client::connect_tcp` API remains the
unbounded compatibility surface. Endpoint selection, retry policy, and service
registration meaning remain application responsibilities.

## Client And Server

`r9p` owns both reusable protocol sides. The server side is the generic session plus backend boundary. The client side is the runtime-neutral operation builder plus response admission boundary. Keeping both sides in one crate is deliberate: tags, fids, stat records, message limits, flush handling, and wire encoding are shared protocol concerns, not application concerns.

The plan9port `9p` command is the client UX target for the one-shot `r9p`
operations. The installed `r9p` binary now also exposes the generic local
communication suite: `mount` for FUSE import, `serve` for local 9P serving
(read-only by default, explicitly writable when requested), and `export` for
serving plus descriptor emission. The reusable core crate remains broader than
that binary and continues to serve embedded clients and servers.

The FUSE mount adapter follows the mature libfuse/Linux concurrency shape: a
bounded worker pool handles kernel requests, and the FUSE INIT reply advertises
bounded `max_background` and congestion settings. This makes recursive walks
and slow peer operations apply backpressure at the mount boundary instead of
spawning unbounded per-request threads in the client process.

The adapter advertises only capabilities it implements. In particular it does
not claim exportfs stale-handle support, because forgotten nodeids are retired,
and it leaves umask application to Linux rather than claiming `DONT_MASK`.

## Non-Goals

- No Racme editor semantics.
- No Vault namespace policy.
- No FUSE/POSIX translation in the protocol core. The workspace's `crates/fuse`
  owns that bridge as an adapter above the core.
- No mandatory async runtime.
- No socket ownership in the protocol core.
- No TLS policy.
- No host-filesystem exporter baked into the library.

## Extraction Rule

`r9p` was seeded inside Racme because Racme needed an Acme-compatible 9P
server first. The extraction trigger was a second real consumer: the FUSE
bridge that is now `crates/fuse` and exposed by `r9p mount`.

`r9p` is one installable communication suite with internal crates for protocol
core, CLI, FUSE bridge, and filesystem-backed serving. Vault-specific
registration lifecycles, listener glue, editor participants, plumbers, and
domain policy remain outside this repository.
