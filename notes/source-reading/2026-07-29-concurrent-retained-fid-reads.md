# Concurrent Retained Fid Reads

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

## Finding

9P already has the required wire semantics. Concurrent requests are
distinguished by tags, while each `Tread` independently carries a fid, offset,
and count. The local r9p server also associates asynchronous cancellation with
the fid, so one `Tclunk` can cancel every outstanding blocking read on that
fid.

The missing piece was only a session-level ownership type. `OpenedFid`
deliberately requires mutable access because it also exposes ordered writes and
same-fid RPC exchange. A separate read-only `ConcurrentReadFid` can safely make
the replay-cursor contract explicit, share one opened fid among readers, and
clunk it exactly once for cancellation.

Application cursor encoding remains a file contract. r9p transports the
ordinary 64-bit offset without assigning service-specific meaning to it.
