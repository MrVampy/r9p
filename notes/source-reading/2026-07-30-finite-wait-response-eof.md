# Finite Wait Response EOF

## Date

2026-07-30

## Question

When a blocking read selects one finite response, when may the server retire
that response without allowing bytes from a later response to be appended to
it?

## Files and Functions Inspected

- `refs/plan9port/src/cmd/9p.c`: `xread`
- `crates/cli/src/io.rs`: `copy_fid_to_writer` and `read_all`
- `crates/core/src/server/read_snapshot.rs`: `FidReadSnapshots::read`
- `crates/front/src/front.rs`: `Front::read_rpc`
- Agents `crates/runner/src/runtime_service/runtime/tree/wait.rs`:
  `publish_response_slice` and `response_slice`

## Source-Backed Findings

- plan9port's `9p read` keeps advancing the offset and reading until it receives
  a zero-length reply.
- r9p's CLI follows the same contract. It does not treat a short non-empty read
  as EOF.
- `FidReadSnapshots::read` pins an ordinary file snapshot so successive reads
  on one fid cannot observe different versions of the file.
- `Front::read_rpc` similarly pins the response selected by a front-end RPC
  read.
- The Agents wait backend retired an implicit finite response as soon as it
  returned the final non-empty chunk. The client's next read, which existed to
  observe EOF, could therefore select a later response and append its suffix.
- A positional event contract may retire a record after its final chunk when
  each record has a distinct explicit cursor and the client advances by cursor
  rather than by seeking EOF.

## Effect on Code and Documentation

- The generic r9p client read loop remains unchanged.
- The Agents implicit wait response stays pinned until a positive-count read at
  or beyond its length returns empty.
- Explicit cursor-keyed terminal update records retain their existing
  final-chunk retirement rule.
- `docs/guides/event-driven-9p.md` now states the finite-record EOF contract.

## Open Questions

None.
