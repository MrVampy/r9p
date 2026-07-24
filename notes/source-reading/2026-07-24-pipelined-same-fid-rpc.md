# Pipelined same-fid RPC

Date: 2026-07-24.

## Question

Can an interactive namespace client remove the idle network round trip between
writing a complete same-fid RPC request and reading its response without
inventing another transport?

## Sources inspected

- `crates/core/src/blocking.rs`
- `crates/core/src/multiplex/client.rs`
- `crates/core/src/server/connection.rs`
- `crates/session/src/client.rs`
- `crates/session/src/opened_fid.rs`
- `crates/auth/src/handshake.rs`
- `crates/reverse/src/lib.rs`
- Agents `crates/runner/src/service/transport.rs`
- Agents `crates/runner/src/service/tree/mod.rs`

## Findings

The bounded TCP client, authenticated Agents listener, and reverse-connect
transport already enable `TCP_NODELAY`. The remaining interactive delay was
therefore not an omitted socket option.

The multiplexed client previously waited for `Rwrite` before sending `Tread`.
For Agents same-fid RPC files, the r9p server connection handles ordinary
writes and reads synchronously in wire order. The write buffers the complete
request before the following read executes it and returns the typed response.
Sending both tagged requests before awaiting either reply preserves that
ordering while removing one otherwise idle round trip.

## Effect

The multiplexed client and retained-fid session facade now expose an opt-in
pipelined write/read operation with bounded delimiter framing. Prefix write
chunks still complete before the final pipelined pair. The API documents that
callers may use it only where the server contract processes the write before
the subsequent read on the same fid.

## Validation

Commit `6372460` passed the complete M7 flake check, including the multiplexed
client test that withholds both replies until it has received the ordered
`Twrite` and `Tread`, then returns the replies out of order to prove tag
demultiplexing.

R9wm adopted the primitive at `d21e1a7`. Three fresh live terminal sessions
then produced 12 steady-state input-to-visible samples: 32.7 ms minimum,
37.8 ms median, 55.1 ms p95, and 57.6 ms maximum. Ordered update timestamps
placed the median input-to-service portion at 24.3 ms and the median
output-to-visible portion at 14.7 ms. The earlier prefetched but sequential
four-command smoke measured 48-59 ms; that small comparison is directional
rather than a controlled benchmark.

All live trials preserved styled Unicode output, completed without an
observation gap, and retired their terminal processes and units. The result
keeps one 9P session and one retained fid while removing the idle dependency
between two requests whose server ordering was already explicit.
