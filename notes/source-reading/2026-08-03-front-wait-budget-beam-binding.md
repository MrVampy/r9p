# Front wait budgets through the BEAM binding

## Date

2026-08-03

## Question

Why did a Dependencies prebuild claim configured to block for one hour return
an r9p RPC timeout every 30 seconds, and which layer owns the repair?

## Sources inspected

- `crates/front/src/front.rs`: `Front::set_wait_timeout` and `read_rpc`.
- `crates/front/src/model.rs`: the 30-second default `State::wait_timeout`.
- `crates/beam-port/src/front_port.rs`: the operations exposed for a BEAM-owned
  front.
- `bindings/gleam/src/r9p/front.gleam`: the public Gleam front surface.
- `crates/session/src/client/paths.rs`: same-fid `rpc_path` behavior.

## Findings

The front already owns a generic configurable wait budget through
`Front::set_wait_timeout`. The same budget bounds pending RPC responses and
other front relay waits, and the default is 30 seconds. The BEAM port did not
expose that existing setter, so a Gleam service could declare an hour-long
application wait while its front still terminated the same-fid RPC after 30
seconds.

This is not a 9P wire change and does not justify application-specific logic in
r9p. It is an adapter completeness gap: the BEAM and Gleam front surfaces must
be able to select the generic wait budget already implemented by the front.

## Effect

Expose `front-set-wait-timeout` through the BEAM port and
`r9p/front.set_wait_timeout` through the Gleam binding. Dependencies can then
set its front budget slightly beyond its declared blocking-claim duration, so
the application deadline responds before the transport deadline. The wait
remains event-driven and cancellable; the timeout remains a bounded liveness
deadline rather than a work-discovery interval.

## Open questions

The setting is front-wide. A future service with several materially different
long-running RPC classes may justify path-scoped wait budgets, but the current
Dependencies front has one long wait class and does not require that added
mechanism.
