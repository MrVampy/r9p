# Multiplexing Blocking Front Requests on BEAM

Date: 2026-07-30

## Question

Can a Gleam service keep a blocking `Front::next_request` intake pending while
an independent BEAM process publishes event-driven projection updates to the
same Front?

## Sources Inspected

- `crates/front/src/front.rs`
  - `Front::set`
  - `Front::append_event`
  - `Front::next_request`
- `crates/beam-port/src/front_port.rs`
  - `FrontManager::handle`
- `crates/beam-port/src/lib.rs`
  - `run_stdio`
  - `PeerClientServer::handle_line`
- `bindings/gleam/src/r9p/front.gleam`
  - `next_request`
  - `run`
- `bindings/gleam/src/r9p_beam_port_ffi.erl`
  - `request_on`
  - `server_loop`
  - `await_response`
- `docs/event-driven-9p.md`
  - blocking subscription and dedicated control-headroom guidance

## Findings

- The Rust `Front` is cloneable and synchronizes its mutable state with a
  mutex and condition variable. A pending `next_request` and a concurrent
  `set` or `append_event` are safe at this layer.
- `r9p-beam-port` previously read one command, completed it synchronously, and
  only then read the next command. A blocking `front-next-request` therefore
  prevented every projection update from reaching the already-concurrent
  Rust Front.
- The Erlang port owner also waited for each native response inside its
  receive loop. Calls from other BEAM processes accumulated behind the same
  request.
- `front.next_request` passed the adapter's ordinary 5-second timeout to the
  port even when the requested intake wait was 60 seconds. An idle but healthy
  blocking intake was therefore classified as a failed port before its own
  timeout could complete.
- Shortening the intake interval would turn the control surface into polling.
  Moving projection state into the control loop would couple unrelated
  service responsibilities and still leave the adapter unable to support
  concurrent callers.

## Effect on r9p

The BEAM port protocol now carries a request ID on every command and response.
The Erlang owner retains all pending callers and demultiplexes replies by that
ID. The native adapter clones a Front only for `front-next-request`, completes
that blocking operation on a worker, and continues accepting projection and
completion commands on the main port loop.

`front.next_request` gives the port the requested wait plus the ordinary
adapter budget. An idle blocking intake can return `front-timeout` normally,
while a genuinely overdue port operation remains bounded.

This is adapter concurrency, not a new 9P message or service-specific policy.
It implements the existing event-driven guidance: a blocking request lane can
remain pending while the same admitted Front retains headroom for status,
events, and control completion.

## Open Questions

- A future adapter can expose explicit cancellation for a BEAM process that
  abandons a pending Front intake before its declared timeout. Current service
  shutdown closes the native port process, and ordinary operation remains
  bounded by the declared wait.
