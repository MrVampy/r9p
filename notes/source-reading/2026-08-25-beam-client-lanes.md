# BEAM Client Lanes

## Date

2026-08-25

## Question

How can an event-driven BEAM consumer keep a blocking namespace RPC pending
without denying ordinary credential, status, or completion RPCs headroom?

## Sources Inspected

- `docs/guides/event-driven-9p.md`
- `crates/beam-port/src/lib.rs`
- `crates/beam-port/src/stdio.rs`
- `bindings/gleam/src/r9p.gleam`
- `bindings/gleam/src/r9p_beam_port_ffi.erl`
- `notes/source-reading/2026-07-30-beam-front-multiplex.md`
- `crates/session/src/client/namespace.rs`

## Findings

- The adapter multiplexes tagged callers through one native client port, but
  ordinary client RPC dispatch remains synchronous in that port. A blocking RPC
  therefore prevents the port from reading later ordinary commands.
- A shorter wait would turn a healthy event-driven claim into polling and would
  not restore independent control headroom.
- The event-driven guide already defines the appropriate boundary: retain one
  admitted connection for ordinary traffic and dedicate another admitted
  connection to blocking work.
- Two explicit client lanes preserve the same caller identity and namespace
  contract while giving each lane an independent native port and cached
  namespace session. This requires no service-specific behavior in r9p.

## Effect

The Gleam adapter can select either the ordinary or blocking client lane. The
Erlang binding owns one native port server for each lane. Consumers place only
their retained blocking operation on the blocking lane and keep credentials,
status, and completion traffic on the ordinary lane.
