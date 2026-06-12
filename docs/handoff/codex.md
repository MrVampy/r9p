# Codex Handoff

## Context

This pass followed Claude's handoff request to review and repair the generic `front` C ABI in `crates/front`. The goal was to make the ABI and serve loop safe enough to be the prototype contract surface for external programs that expose state over 9P.

Relevant source references checked:

- `docs/source-map.md`
- `crates/front/src/abi.rs`
- `crates/front/include/r9p_front.h`
- `crates/front/src/lib.rs`
- `crates/front/src/serve.rs`
- `crates/front/tests/conformance.rs`
- `crates/front/demo/front_host.ts`
- `crates/core/src/server/handlers.rs`
- `crates/core/src/server/types.rs`
- `crates/core/src/flush.rs`
- `refs/plan9port/src/lib9p/srv.c`

## Changes Made

- Bumped the front C ABI to version 2.
- Changed `r9p_front_request_copy` to take `request_id`, removing the single shared staging slot race.
- Made front request ids globally unique within a front instance, so keyed request copies are unambiguous across intakes.
- Updated the public C header and Deno demo for ABI v2.
- Clarified the handle lifetime contract: calls other than `r9p_front_free` are thread-safe; `free` is the lifetime boundary and must happen after all in-flight calls have returned.
- Moved 9P fid bookkeeping from shared front state into `FrontTree`, making fids per connection instead of global across connections.
- Tightened open-mode checks: read-only files and logs accept only `OREAD`; intake `new` accepts only `OWRITE`; `OEXEC`, `ORDWR`, `OTRUNC`, `ORCLOSE`, and unknown flags are rejected.
- Fixed directory reads at nonzero offsets by returning directory entries to the core server and letting `dirread_chunk` apply offset/count chunking.
- Reworked `serve_tcp` from synchronous `Server::handle` to split `admit`/`complete` handling for async reads.
- Added per-read cancellation tokens so `Tflush` wakes a parked log read and suppresses the stale read completion.
- Capped inbound TCP frame allocation at `codec::MAX_MSIZE`.
- Made `r9p_front_stop` and `r9p_front_free` stop and join tracked accept threads.

## Verification

- `cargo fmt -p front`
- `cargo test -p front`
- `cargo clippy -p front --all-targets`

The front test suite now includes regressions for:

- keyed ABI request copies after two pending requests;
- directory reads continued from a nonzero offset over TCP;
- same-connection `Tflush` interrupting a blocked log read;
- exact open-mode behavior;
- per-connection fid isolation.

## Remaining Notes

- `Cargo.lock` was already dirty before this pass with the new `front` package entry. I did not treat that as part of this repair.
- The C ABI still uses an ordinary opaque pointer handle. That means fully arbitrary post-free stale pointer calls cannot be made safe without changing the handle model to an integer/table token or intentionally leaking tombstone tokens. The repaired contract instead states the normal C lifetime rule explicitly: `r9p_front_free` is not concurrent with other calls and must be the final use of the handle.
- Existing connected 9P sessions are not joined by `r9p_front_stop`; stop halts accepting and joins the accept thread. Connection threads exit when their clients close or the serve loop stops reading.
