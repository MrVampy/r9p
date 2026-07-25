# Retained fids for interactive channels

Date: 2026-07-23

## Question

At what granularity should an interactive application use 9P after it has
established a session?

## Sources inspected

- `refs/plan9port/include/9pclient.h`
  - `CFid`, `fsopen`, `fsread`, `fswrite`, and `fsclose`
- `refs/plan9port/src/lib9pclient/read.c`
- `refs/plan9port/src/lib9pclient/write.c`
- `refs/plan9port/src/lib9pclient/close.c`
- `crates/core/src/multiplex/client.rs`
  - `walk_timeout`, `open_timeout`, `read_full_timeout`, `write_timeout`, and
    `clunk_timeout`
- `crates/core/src/fid.rs`
  - `FidState`
- `crates/session/src/client.rs`
  - the session-level client facade

## Findings

- The established plan9port client API treats an opened fid as a retained
  handle. Callers read and write through the same `CFid` and close it when the
  logical channel is finished.
- A 9P connection is already multiplexed by tags. Rewalking and reopening a hot
  file for every small application message is not required by the protocol.
- The r9p multiplexed client already supports repeated reads and writes on an
  opened fid, but the session crate previously exposed only the primitive fid
  operations. Applications therefore tended to repeat walk, open, and clunk.
- File-level ordering remains an application concern. A retained-fid facade
  should require mutable access so overlapping operations on the same logical
  file cannot happen accidentally.

## Effect on r9p

The session crate now owns a generic `OpenedFid` facade and
`Client::open_path_timeout`. Interactive consumers can retain one opened file
per hot logical channel while continuing to use ordinary 9P messages.

This utility has no coordinator, agents, terminal, or application policy. The
application still chooses which files are channels, how messages are framed,
and when a fid should be replaced.

## Open questions

- A future asynchronous session facade may need an owned channel abstraction,
  but it should preserve the same fid lifecycle and avoid imposing an async
  runtime on the reusable protocol core.
- Referral refresh is separate from retained-fid lifetime. A finite referral
  governs when a new session may be established; it does not require reopening
  a fid for every message.
