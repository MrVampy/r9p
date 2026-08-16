# r9p Architecture

`r9p` is the reusable 9P protocol primitive. It is intentionally narrower than a filesystem, narrower than a FUSE bridge, and narrower than any one application.

## Boundary

```text
backend
  Racme Acme tree, coordinator adapter, exportfs-style host tree, memory fixture

r9p server core
  9P messages, qids, fids, tags, stat records, version negotiation,
  walk/open/read/write/clunk/flush lifecycle and response bounds

optional r9p connection facade
  checked frames, split admission/completion, bounded cancellable workers

transport adapter
  Unix socket, TCP, stdio, BEAM port, virtio transport

r9p client core
  9P operation builders, response admission, transparent referral routing

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

The reverse-connect adapter follows the same backend boundary.
`ReverseExport` owns authenticated outbound connection pooling, bounded
reconnect, shutdown, and one ordinary 9P server session per accepted stream.
Its caller supplies a fresh `FileTree` factory, so an application tree does not
have to impersonate a host filesystem to use reverse attachment.
`FilesystemExport` is the convenience specialization that opens `LocalTree`
instances; the broker remains unaware of either tree. The transport adapter
also owns bounded TCP keepalive for application-idle pool streams, so hard peer
loss becomes a stream failure and re-enters the existing reconnect lifecycle.

Reverse placement authentication and end-service authentication are distinct.
The first authenticates the exporter to the broker and protects the outbound
placement stream. `SecureStream` remains a cloneable authentication transport,
so `ReverseExport::start_authenticated` and
`ReverseExport::start_authenticated_handler` can establish a second
p9any/Noise session through that stream before 9P version negotiation. The
second session proves the final service client and supplies the server's fixed
attach identity. This closes the loopback proxy boundary without making the
broker an application admission or protocol relay.

When an application authorizes certificate groups,
`start_authenticated_handler_with_peer` supplies the verified `PeerIdentity`
to the handler factory before the first 9P request. The transport proves and
delivers identity; the application handler decides authorization.

The broker proxy is local by default. `ProxyExposure::AuthenticatedNetwork`
admits a concrete non-loopback TCP listener only for a deployment that uses
the authenticated reverse-export variants above. This is an explicit claim
that the bridged end service authenticates every final client. It does not move
identity admission or namespace policy into the broker.

The server core owns wire-level fid lifecycle. It records open modes and
directory offsets, rejects operations that violate 9P sequencing, and reserves
fid transitions across split request completion. Clone walks share a source
reservation so independent walks can overlap, while open, create, clunk,
remove, in-place walk, and version reset cannot race that source transition.
Backends still own namespace meaning, permissions, content, and application
effects. A `FileTree` reset clears backend session-local state when `Tversion`
starts a new session.

Dynamic ordinary files can use `FidReadSnapshots` for bounded, coherent byte
reads across multiple `Tread` ranges on one opened fid. The backend still
chooses which files have snapshot semantics; waits, streams, and other live
files remain live. The shared helper owns byte accounting, range slicing, and
fid retirement without importing path or application policy into the core.
Front-backed applications whose record is produced asynchronously use
`register_snapshot_read_relay`: the first read asks the owner for one complete
record, the Front pins that response to the opened fid, and later ranges
including explicit EOF are answered without another owner request. The
ordinary `register_read_relay` remains the distinct range-at-a-time contract.

Demand-backed directories use `register_directory_read_relay`. When a read
reaches the observed end of an opened fid, Front asks the owner for another
page. The owner first materializes direct children and then completes the
request with their names and an EOF fact. Front validates those names, appends
their stats to that fid's ordered snapshot, and retains responsibility for 9P
directory encoding and byte offsets. Other fids progress independently and may
reuse application-owned page caches without sharing their read cursor.

The core owns `Twstat` fields whose mutability is fixed by 9P2000. Immutable
fields and file-type changes are rejected before backend dispatch. The backend
then validates its complete mutable-field policy before changing storage and
must provide all-or-none completion. A backend that cannot make a requested
combination atomic rejects it; it must not apply a prefix of the request.

Variant negotiation is capability negotiation, not a label. Plain `9P2000`
never exposes symlink qids, symlink stat bits, or namespace referrals.
`9P2000.R` is the r9p dialect. It admits `QTSYMLINK` and `DMSYMLINK`, the
existing read-target representation used by the filesystem exporter and FUSE
bridge, and the `Treferrals` and `Rreferrals` messages used for transparent
namespace composition. It also adopts the exact 9P2000.L `Trenameat` wire
shape as its single owner-atomic cross-parent rename operation. This does not
claim the rest of 9P2000.u or 9P2000.L. Servers configured for the dialect can
downgrade a plain requester, while extension-aware clients reject extended
operations or metadata after a downgrade. P9any and Noise authenticate the
stream before version negotiation and are not part of the dialect.

`r9p` provides generic client create, write, remove, read, and RPC operations,
plus language bindings that encode the transport-neutral `r9p-export.v1`
descriptor. An application that registers with a runtime owns the lifecycle
that writes that descriptor through the runtime's ordinary namespace. The
governing application owns service admission, lease interpretation, and
projection. Neither side gives `r9p` coordinator-specific registration policy
or a privileged runtime control path.

The blocking TCP client has an opt-in bounded connection seam:
`Client::connect_tcp_with_timeouts` takes independent connect, read, and write
timeouts. It resolves the endpoint, uses `TcpStream::connect_timeout` for each
resolved address, and installs the read and write timeouts before 9P version
negotiation and attach. `Client::connect_tcp` is the unbounded convenience
surface. Endpoint selection, retry policy, and service registration meaning
remain application responsibilities.

## Client And Server

`r9p` owns both reusable protocol sides. The server side is the generic session plus backend boundary. The client side is the runtime-neutral operation builder plus response admission boundary. Keeping both sides in one crate is deliberate: tags, fids, stat records, message limits, flush handling, and wire encoding are shared protocol concerns, not application concerns.

The client runs on the host of the process that consumes the namespace. After
attaching to a `9P2000.R` root, it requests finite admitted referrals, selects
the longest matching mounted prefix, and establishes direct service sessions
inside the same caller process. Referrals are 9P messages, not public namespace
files. Callers continue to walk, open, read, write, and issue RPCs against one
logical namespace.

A referral is interpreted in the caller's network and authority context.
Moving the client onto the service host changes the caller and does not prove
that the original host can resolve, authenticate, or connect. In particular, a
loopback endpoint is valid only for a caller on that same host. If a remote
caller receives one, the composition is incomplete and must be fixed through
reachable authenticated publication, reverse attachment, or an explicit
caller-local authority binding. SSH remains a host-administration mechanism;
it is never a substitute transport for an r9p namespace client.

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

Namespace-change observation is stream-primary. Enabling a change feed requires
both a recent or cursor-addressed catch-up path and a blocking stream path. The
client blocks on the stream for new records. After transport loss it reads the
catch-up path once from its last event ID, but only after opening the replacement
stream so changes arriving during catch-up are retained. There is no periodic
snapshot fallback; the configured delay is reconnect backoff, not an
observation cadence. Read deadlines remain bounded liveness and shutdown
limits.

The session layer also provides a reusable bounded coherent local
materialization for consumers that need a complete native read tree. It opens
the same stream before taking its initial parallel snapshot, incrementally
publishes ordinary file changes, and reconstructs the derived tree after any
coarse invalidation. FUSE remains lazy and translates the shared feed events
into kernel cache invalidations; it and the eager local materializer share one
strict decoder, cursor, reconnect, and backpressure implementation rather than
duplicating feed behavior. The eager materializer retains a strict internal
cursor beside its derived tree. A restart opens the stream first, catches up
from that cursor, and reuses the local tree only when the exact materialization
configuration and bounded tree are still valid. Cursor publication follows a
durable complete feed batch, never an individual path record. Missing,
malformed, stale, or unprovable state falls forward to a full snapshot; the
derived tree never becomes a second authority.

The generic pattern is described in
[`event-driven-9p.md`](../guides/event-driven-9p.md). A retained blocking read is the
subscription. Implicit per-fid streams admit one outstanding read per fid;
explicit positional feeds may pipeline independent tagged reads when their file
contract makes offsets replay-safe. Cancellation flushes each outstanding tag
before clunking the fid.

The adapter advertises only capabilities it implements. In particular it does
not claim exportfs stale-handle support, because forgotten nodeids are retired,
and it leaves umask application to Linux rather than claiming `DONT_MASK`.

## Non-Goals

- No Racme editor semantics.
- No coordinator namespace policy.
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
core, CLI, FUSE bridge, and filesystem-backed serving. coordinator-specific
registration lifecycles, listener glue, editor participants, plumbers, and
domain policy remain outside this repository.
