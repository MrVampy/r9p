# FUSE Cache Read Pipelining

## Question

Why could a complete retained media artifact stream too slowly through an r9p
FUSE mount even though the persistent cache was healthy and evicting old chunks
correctly?

## Sources inspected

- `crates/core/src/multiplex/client.rs`
- `crates/core/src/server/connection.rs`
- `crates/fuse/src/fuse/ops/io/read.rs`
- `crates/fuse/src/fuse/read_cache.rs`
- `crates/session/src/client/namespace/operations.rs`
- `../vault-apps/newsgroups/src/artifacts.rs`

## Findings

The persistent read cache filled one 4 MiB cache chunk by calling the
multiplexed client in a serialized `read_full_timeout` loop. The negotiated 1
MiB transport limit therefore cost several complete network round trips before
a cache miss could return any bytes.

Live measurement separated this from local cache work. A full 32 GiB cache
scan took 23 ms and a synced 4 MiB cache write took 11 ms, while a cold 32 MiB
FUSE read took 29 seconds. The cache was already at its quota and correctly
removed an old chunk for each new chunk without read or write errors. Eviction
was not the throughput bottleneck.

The Newsgroups service exposes positional read-only artifact relays and the
core 9P server already dispatches reads asynchronously. Bounded concurrent
positional reads on the same open fid therefore preserve semantics and use the
existing multiplexed connection as designed. There is no reason to create a
second endpoint or make MPV aware of Newsgroups.

## Decision

`MultiplexedClient` owns exact bounded positional pipelining beside its existing
`read_full_timeout` operation. Direct and logical namespace clients delegate
without reimplementing it. FUSE selects it when a persistent cache miss needs
to fill a complete known-length chunk.

Cache fills partition each chunk at negotiated transport boundaries and keep
at most eight ordered ranges in flight over the same fid. Results are joined in
offset order, every submitted read is resolved or cancelled before an error is
returned, and every range must be complete before cache publication. Mutation
operations are unchanged.

The protocol regression requires a complete read window to arrive before the
server sends any response, returns the responses in reverse order, and proves
the client assembles bytes by offset. A second regression proves an incomplete
response cannot make the client return while another submitted read remains
unresolved.
