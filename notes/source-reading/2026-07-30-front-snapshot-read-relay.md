# Front Snapshot Read Relay

## Date

2026-07-30

## Question

How should a Front-backed service publish a dynamic finite record without
rendering or relaying every byte range independently and without concatenating
different record revisions before explicit EOF?

## Files and Functions Inspected

- `refs/plan9port/src/cmd/9p.c`: `xread`
- `crates/cli/src/io.rs`: `copy_fid_to_writer` and `read_all`
- `crates/core/src/server/read_snapshot.rs`: `FidReadSnapshots`
- `crates/front/src/front.rs`: `Front::register_read_relay`,
  `Front::complete_request`, and `Front::response_read`
- `crates/front/src/tree.rs`: `FrontTree::read_target_at` and
  `FrontTree::clunk`
- `crates/front/src/tests.rs`:
  `read_relay_dispatches_each_range_and_consumes_its_response`
- Agents `crates/runner/src/profile_service/front.rs`: `status_loop`

## Source-Backed Findings

- Both plan9port `9p read` and r9p's one-shot read keep advancing the offset
  until a zero-length response. A short non-empty response is not EOF.
- `register_read_relay` intentionally forwards every range independently. Its
  owner receives the client's offset and count and its response is consumed by
  that one `Tread`.
- A finite dynamic report has different semantics. One rendered record must be
  pinned to the opened fid across every range and the explicit EOF read.
- `Front::complete_request` already owns an asynchronous response slot whose
  bytes can remain available. Pinning that response ID to the FrontTree's fid
  gives the asynchronous Front the same coherence boundary that
  `FidReadSnapshots` gives a synchronous backend.
- A response slot must accept exactly one completion. Allowing a second
  completion before clunk would mutate the supposedly pinned record.
- Reinterpreting the existing range relay as a snapshot would silently break
  legitimate range-at-a-time owners. The two semantics need separate explicit
  registration primitives.

## Effect on r9p

- `register_snapshot_read_relay` is the finite-record primitive. The first
  read on an opened fid enqueues one owner request, `complete_request` supplies
  the entire record, and every later byte range including EOF reuses it.
- Clunk, reset, and connection teardown retire the pinned response.
- A second completion or rejection of the same response slot is rejected.
- `register_read_relay` retains its existing range-at-a-time behavior.
- The Rust Front, C ABI, BEAM port, and Gleam binding expose the same primitive.

## Open Questions

None.
