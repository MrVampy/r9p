# Create And Initial Write Fid Lifetime

## Question

Where should a generic create-and-initial-write operation live when a 9P server requires the first write on the fid returned by `Tcreate`?

## Sources Inspected

- `refs/plan9port/include/9pclient.h`
- `refs/plan9port/src/lib9pclient/create.c`
- `crates/core/src/blocking.rs`
- `crates/front/src/abi/client.rs`
- `crates/beam-port/src/lib.rs`
- `bindings/gleam/src/r9p.gleam`

## Findings

Plan 9's client interface returns a fid from create, and r9p's blocking client already preserves the protocol rule that `Tcreate` reuses an open fid for the new file. A one-shot `create_at` necessarily clunks that fid, so a later path-based write cannot preserve per-fid create state.

Create plus initial write is generic 9P client mechanics. It belongs in `blocking::Client`, with the C ABI and BEAM adapter delegating to that implementation. Vault service-registration naming, descriptor fields, renewal timing, and retry policy remain app-owned.

## Effect

- Add `blocking::Client::create_write_at`.
- Delegate `r9p_front_client_create_write_at` to the blocking client.
- Expose `create-write-at` through the BEAM port and Gleam adapter.
- Keep `/srv` and service-registration semantics out of r9p.

## Open Questions

None.
