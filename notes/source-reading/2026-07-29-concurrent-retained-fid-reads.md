# Concurrent Retained Fid Reads

Date: 2026-07-29

## Question

Can one opened 9P fid carry several concurrent blocking reads when each read
offset is an independent application cursor?

## Sources checked

- `crates/core/src/multiplex/client.rs`
  - `MultiplexedClient::read_delimited_timeout`
  - `MultiplexedClient::read_timeout`
- `crates/core/src/multiplex/reader.rs`
  - the response reader demultiplexes replies by request tag
- `crates/core/src/server/connection.rs`
  - `serve_loop`
  - `dispatch_async`
  - fid-scoped cancellation during `Tclunk`
- `refs/plan9port/src/lib9p/req.c`
  - request lifetime is tag-scoped and holds a reference to its fid
- `refs/plan9port/src/lib9p/srv.c`
  - `sread` accepts the request's explicit offset for an opened non-directory
    fid
- `refs/plan9port/include/9pclient.h`
  - `fspread` exposes positional reads independently of the seek cursor
- `../r9wm/refs/9front/sys/man/5/0intro`
  - clients may send multiple tagged requests without waiting for replies, and
    servers may reply out of order
- `../r9wm/refs/9front/sys/man/5/flush`
  - `Tflush` is the protocol cancellation operation for a pending request tag
- `../r9wm/refs/inferno-os/appl/cmd/ndb/registry.b`
  - its implicit-cursor event file rejects a concurrent read on one fid
- `refs/knusbaum-go9p/client/client.go`
  - closing a file flushes every outstanding tag associated with its fid before
    clunking it
- `refs/knusbaum-go9p/fs/stream_file.go`
  - each open fid receives its own implicit stream reader
- `NERVsystems/llm9p`, commit `42c2e6958db4e870f21ce0b60b7522975cd8757f`
  - an LLM output file blocks its read on the next generated chunk and ignores
    offsets as an implicit stream
- `Barre/ZeroFS`, commit `d2bc8b9cdedea9384443267cd4a5753d639b148d`
  - a current Rust 9P server treats `Tflush` and `Tclunk` as explicit request
    and fid barriers

## Finding

9P already has the required wire semantics. Concurrent requests are
distinguished by tags, while each `Tread` independently carries a fid, offset,
and count.

The references distinguish two file contracts. An implicit event stream keeps
its cursor on the fid and ordinarily permits one outstanding read per fid.
Independent observers open independent fids. A positional event file instead
puts an explicit cursor in each `Tread` offset; those reads may safely remain
concurrent on one fid because they do not mutate a shared seek position. The
terminal update file is the second kind.

The missing piece was only a session-level ownership type. `OpenedFid`
deliberately requires mutable access because it also exposes ordered writes and
same-fid RPC exchange. A separate read-only `ConcurrentReadFid` can safely make
the replay-cursor contract explicit and share one opened fid among readers.
Cancellation remains tag-scoped: it snapshots the outstanding tags, sends and
awaits `Tflush` for each, and only then sends `Tclunk`. Relying on clunk alone
would work with r9p's current server but is not the portable 9P cancellation
contract.

Application cursor encoding remains a file contract. r9p transports the
ordinary 64-bit offset without assigning service-specific meaning to it.

## Effect

- Added `docs/event-driven-9p.md` as the generic application guide.
- Kept implicit-stream and positional-feed semantics out of the wire dialect.
- Made concurrent retained reads track their outstanding tags.
- Made cancellation await `Rflush` for every outstanding read before
  clunking the fid.

## Open Questions

- The useful in-flight read depth remains a measured consumer choice. It is
  not fixed by this protocol finding.
