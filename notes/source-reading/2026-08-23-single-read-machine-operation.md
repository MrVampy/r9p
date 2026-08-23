# Single-read machine operation

## Sources checked

- `crates/session/src/client/namespace/operations.rs`: `Client::read` issues
  one logical read through the routed direct session.
- `crates/core/src/multiplex/client.rs`: `Client::read` clamps the requested
  count to the negotiated message size and issues one `Tread`.
- `crates/cli/src/commands/script.rs`: `read-hex` repeatedly reads until it
  fills the requested range or receives an empty response.
- `refs/plan9port/src/cmd/9p.c`: the ordinary `9p read` command loops until
  end of file, matching r9p's existing range and stream commands.

## Finding

A blocking append file may return the currently available event batch from one
`Tread` and then block the next read at the new tail. A range-filling command
therefore cannot expose that first batch until the entire requested range is
filled. This is correct for finite range reads but wrong for a consumer that
must process each available append batch before waiting again.

## Decision

Keep `read-hex` unchanged and add `read-once-hex` to the machine script
surface. The new operation opens one fid, issues exactly one routed `Tread`,
prints that response as bounded hexadecimal, and clunks the fid. It adds no
application cursor or event semantics to r9p.
