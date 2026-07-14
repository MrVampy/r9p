# Synthetic Read Relay

Date: 2026-07-14.

## Question

How should a long-running application expose a file whose bytes are loaded only
when a 9P client reads it?

## Sources Inspected

- `refs/plan9port/include/9p.h`: `Srv.read`, `Req`, `Fid`, `readstr`, and
  `readbuf`.
- `refs/plan9port/man/man9/read.9p`: `Tread` offset and count semantics.
- `refs/plan9port/src/cmd/mntgen.c`: generated walks, reads, and fid cleanup in
  a synthetic file server.
- `crates/front/src/tree.rs`: current static-file, log, RPC, and relay request
  dispatch.
- `crates/front/src/front.rs`: response waiting, cancellation, timeout, and
  request completion.
- `crates/front/src/serve.rs`: concurrent read dispatch and flush cancellation.
- [plan9port `9p(3)`](https://9fans.github.io/plan9port/man/man3/9p.html):
  published `Srv.read(Req*)` and asynchronous response contract.
- [plan9port `mntgen.c`](https://github.com/9fans/plan9port/blob/master/src/cmd/mntgen.c):
  upstream synthetic-tree implementation.

## Findings

A lazily materialized object is still an ordinary read-only 9P file. Each
`Tread` carries an offset and count to the server read callback. The callback
returns at most that range or responds with an ordinary 9P error. It may defer
the response to another process, provided flush can cancel the outstanding
request.

The application owns materialization and cache policy. The protocol adapter
should not recast a read-only file as a write-then-read RPC, and it should not
impose a whole-file cache across reads. plan9port also recommends generating a
highly structured tree as needed when authoritative state is maintained
elsewhere.

## Decision

The front adapter gains a generic read relay. Registering a path creates a
read-only synthetic file. Every read enqueues one application request carrying
offset and count. Completion supplies bytes for that read, rejection supplies
the 9P error, and the response is consumed immediately. Existing flush and
timeout handling owns abandoned requests.

The public request context advances to `r9p-front-request-context.v2` and the C
ABI advances to version 19. The BEAM and Deno adapters move directly to the new
contract; no compatibility decoder is retained.

## Open Questions

None for immutable and otherwise application-owned synthetic files. A future
application that wants cross-read caching should implement that policy above
the read relay or expose a separate backend optimized for its storage model.
