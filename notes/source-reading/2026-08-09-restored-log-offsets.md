# Restored Log Offsets

Date: 2026-08-09.

## Question

How can a service reconstruct a bounded append-only Front log after process
restart without invalidating durable byte cursors held by namespace clients?

## Sources inspected

- `crates/front/src/model.rs`: `LogBody`, `LogBody::append`, and
  `LogBody::read`.
- `crates/front/src/front.rs`: `Front::append_event`,
  `Front::register_log`, and `FrontTree::read_log`.
- `crates/front/src/tests.rs`: retained-window and absolute-offset tests.
- `crates/beam-port/src/front_port.rs` and
  `bindings/gleam/src/r9p/front.gleam`: the BEAM Front boundary used by
  Dependencies.
- `crates/front/src/abi/mod.rs`, `crates/front/include/r9p_front.h`, and
  `crates/front/bindings/deno/front_sink.ts`: the other public Front
  bindings.
- `vault-apps/dependencies/src/dependencies/namespace.gleam`: reconstruction
  of `/dependencies/journal` from durable retained deliveries.

## Findings

`LogBody` already keeps an absolute `start` offset while it evicts complete
entries from a live bounded window. Reads before that offset fail with the
typed earliest retained offset, and `stat.length` reports the absolute end.

The public construction API could only create a log at offset zero. A durable
service therefore reconstructed the same retained records at a new zero-based
lineage after every process restart. A client holding an older durable offset
could then land beyond the reconstructed end or in the middle of a JSON
record. Transport reconnect and fid replay cannot repair an application
cursor whose publisher silently changed its coordinate system.

Plan 9 read offsets do not require zero to be the first retained application
offset. The Front already exposes a synthetic append-only log rather than an
ordinary disk file, so restoring its absolute start is backend state, not a
wire-protocol extension.

## Effect

Add `Front::register_log_at(path, start_offset)` and expose it through the
BEAM, C, and Deno Front bindings. The existing `register_log` remains the
ordinary zero-origin constructor. Durable publishers can now reconstruct a
bounded retained window under its prior absolute byte lineage, while clients
continue to use ordinary 9P reads and offsets.

Dependencies will persist the absolute journal end with its retained delivery
window, derive the retained start at boot, register the log at that offset,
and append the retained records in chronological order.

## Open questions

A consumer that remains offline until its cursor falls before the bounded
window still needs a service-owned catch-up snapshot or an explicit
application rebaseline. Restoring offsets solves restart continuity; it does
not turn bounded retention into infinite history.
