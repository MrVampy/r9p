# Fid read snapshots

Date: 2026-07-27

## Question

Where should multi-range read coherence for mutable ordinary namespace files
live, and how should an application distinguish those files from live waits or
streams?

## Sources inspected

- `crates/core/src/server/mod.rs`
  - `FileTree`
  - `Server`
- `crates/core/src/server/handlers.rs`
  - `perform_file_tree_request`
- `crates/core/src/server/session.rs`
  - `Session`
- `crates/core/src/server/stream.rs`
  - `Stream`
  - `Broadcaster`
- `crates/front/src/tree.rs`
  - `FrontTree::open`
  - `FrontTree::read`
  - `FrontTree::clunk`
- `../coordinator/src/native/r9p_listener/src/lib.rs` at coordinator commit
  `ca061a08f9f6a96cf4b6ec1e422696a9b9123757`
  - `RemoteHandle`
  - `RemoteBackend::read_snapshot_for_open`
  - `RemoteBackend::read`
- Agents `crates/runner/src/compute_service/runtime/tree/mod.rs`
  - `ComputeTree::prepare_read`
  - `ComputeTree::read_snapshot`
  - `FileTree for ComputeTree`

## Findings

- A 9P client may need several `Tread` requests to consume one file. Rendering
  a mutable report independently for each range can splice different report
  revisions into one invalid byte sequence.
- Fid lifetime is the correct coherence boundary. A snapshot must survive EOF
  probes and remain stable until the fid is walked over, clunked, or reset.
- The choice between snapshot and live semantics is application policy.
  Ordinary reports use snapshots, while wait and stream files must continue to
  observe changes after open.
- Byte accounting, range slicing, capture uniqueness, and fid retirement are
  backend-neutral server mechanics. Keeping copies in each service creates
  different capacity and EOF behavior.
- The generic `FileTree` trait deliberately gives backends the fid on open,
  read, and clunk, so a bounded helper composes with synchronous trees and
  custom connection handlers without adding runtime or path policy to r9p.

## Effect

`r9p::server::FidReadSnapshots` now owns the generic bounded per-fid byte store
and slicing behavior. Applications select the ordinary files that capture into
it and explicitly exclude live waits and streams. Agents consumes this helper
instead of maintaining a service-local map and byte counter.

## Open questions

None for this extraction. A future backend may choose eager capture at
`Topen`; lazy capture on the first `Tread` remains coherent for all ranges on
that opened fid and avoids rendering files that are opened but never read.
