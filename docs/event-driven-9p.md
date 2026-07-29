# Event-Driven Work in 9P

9P is a request-response protocol, but a 9P application does not have to poll.
The established event-driven pattern is to represent an event source as a file
whose `Tread` remains pending until data is available.

This document explains the protocol pattern, the two important cursor
contracts, cancellation, backpressure, and reconnect behavior. It applies to
terminal frames, journals, process output, namespace changes, agent events, and
other live sources. The application decides what an event means. r9p supplies
the generic protocol and session mechanics.

## Core Pattern

A client walks to an event file, opens it once, and retains that fid. It then
issues a read that the server may complete immediately or hold until an event
exists:

```text
client                                      server

Twalk -> Topen
Tread tag=41, fid=7, offset=cursor
                                            wait for an event
                                            event becomes available
                                  Rread tag=41, data=record
Tread tag=42, fid=7, offset=next-cursor
                                            wait for an event
```

The waiting `Tread` is the subscription. No separate subscribe message, public
socket protocol, or polling interval is required.

The event source should wake a pending read through a condition variable,
channel, descriptor readiness notification, or an equivalent blocking
primitive. A periodic sleep that checks for new work is polling, even when it
is hidden behind a 9P file.

## Retain the Fid

Walking, opening, reading, and clunking for every event is valid 9P, but it
serializes several request-response round trips before each observation. That
turns network latency and jitter into event-delivery latency.

The normal live-file shape is:

1. Walk and open once.
1. Retain the opened fid for the observation lifetime.
1. Keep at least one `Tread` pending while more events are wanted.
1. Flush pending reads during cancellation.
1. Clunk the fid after no read remains pending.

A fid is scoped to one 9P connection. Retaining a fid does not make it survive
a disconnected or replaced session.

## Two Cursor Contracts

The file contract determines whether one or several reads may be pending on the
same fid. 9P supplies the fid, offset, count, and tag; it does not assign event
cursor semantics to them.

### Implicit Cursor Per Fid

An implicit stream keeps its read position in server-side state associated with
the opened fid. Each successful read advances that position.

This is the traditional shape used by Acme event files and many stream-file
helpers:

- Keep one outstanding read per fid.
- Open the file again to create an independent observer.
- Do not issue concurrent reads on one fid unless the file explicitly defines
  their ordering.
- The `Tread` offset may be ignored because the fid owns the cursor.

Inferno's registry event implementation makes this constraint explicit by
rejecting a concurrent event read on the same fid.

### Explicit Positional Cursor

A positional event file interprets every `Tread` offset as a complete,
independent cursor. For example, an offset may identify an exact event
sequence, plus a byte position within that event record.

This contract permits several tagged reads to remain pending on one fid when:

- each offset identifies an independent observation;
- a read at that offset is replay-safe;
- replies may arrive out of order without changing their meaning; and
- the server retains enough history to answer the accepted cursor window.

9P explicitly permits a client to send several requests with distinct tags
without waiting for earlier replies. The server may reply out of order, and
the tags demultiplex those replies. Pipelined positional reads are therefore a
valid 9P specialization. They are not a universal rule for every event file.

The number of reads kept in flight is a bounded look-ahead and backpressure
choice, not a 9P constant. A depth such as eight should be justified by event
cadence, network delay and jitter, server request capacity, retained history,
and measured saturation. Increasing it does not create more events.

Do not consume a connection's entire asynchronous request capacity with
blocking reads if that connection must also carry metadata or mutation
requests. A client can reserve explicit headroom, or it can dedicate one
connection to the blocking-read window and retain another admitted connection
for status and control. The latter keeps stream backpressure independent of
interactive control without introducing another protocol. The read lane is
still ordinary 9P, uses the same caller identity and namespace contract, and
must be cancelled through `Tflush` before it is closed.

## Cancellation

The request tag is the protocol cancellation identity.

For each pending read:

1. Send `Tflush` with the read tag as `oldtag`.
1. Await the matching `Rflush`.

After all pending read tags have crossed that barrier, send `Tclunk` for the
fid and await `Rclunk`.

`Rflush` means the server has either replied to the original request or will
not reply to it. Waiting for it prevents a late read reply from racing fid
retirement. A server may also choose to cancel work when it sees `Tclunk`, but
clunk-only cancellation is an implementation extension, not the portable 9P
cancellation contract.

The server-side wait must observe cancellation promptly. In r9p's connection
facade, `Tflush`, `Tclunk`, version reset, and connection teardown signal the
associated cancellation state independently of the bounded asynchronous
worker capacity.

## Backpressure and Retention

Event-driven does not mean unbounded.

The client bounds:

- pending tagged reads;
- maximum bytes requested per read;
- decoded records waiting for its consumer; and
- reconnect attempts and liveness deadlines.

The server bounds:

- asynchronous requests per connection;
- retained event history;
- bytes retained per fid or cursor range; and
- the work admitted for one principal or session.

When a requested cursor is older than retained history, the file contract
should report a precise resynchronization condition. It should not silently
return a newer event because that would turn data loss into apparently valid
progress.

A timeout can bound liveness or shutdown. It must not become a timer used to
discover whether an event exists.

## Reconnect and Replay

Tags and fids end with their connection. After transport loss, a resumable
observer must:

1. Establish a new authenticated 9P session.
1. Reattach, walk, and reopen the stable namespace path.
1. Resume from the last application cursor known to be consumed.
1. Use a catch-up source when live-stream retention cannot cover the gap.
1. Return to blocking live reads.

This is session reconstruction, not persistence of the old fid.

Replay is safe only when the file contract says the cursor is replay-safe.
Ordinary writes and mutations must not be repeated automatically merely
because their replies were lost.

For a durable feed, a useful service shape is a cursor-addressed catch-up file
plus a blocking live file. Catch-up is used once after a gap or reconnect; it
is not periodically sampled as a fallback.

## r9p Surfaces

The r9p layers preserve these boundaries:

- The protocol client owns tags, request multiplexing, `Tflush`, and response
  demultiplexing.
- A pending read exposes its tag so cancellation remains protocol-correct.
- `ConcurrentReadFid` is an explicit opt-in for a replay-safe positional file.
  It is not appropriate for an implicit per-fid stream.
- `ResumableFid` reconstructs a session only for paths and offsets whose
  application contract permits replay.
- Feed adapters use blocking live reads and cursor-based catch-up without
  periodic polling.
- Backends own event meaning, cursor encoding, history retention, and gap
  policy.

## Design Checklist

Before exposing a live source through 9P, answer:

1. Is the cursor implicit in the fid or explicit in every read offset?
1. Can several reads on one fid complete independently and out of order?
1. What is the event record boundary and maximum size?
1. How many pending reads and retained records are bounded?
1. What exact error tells the client that its cursor is too old?
1. How does `Tflush` wake the blocked server work?
1. What application cursor is committed before reconnect?
1. Which operations are replay-safe?
1. What catch-up source covers a retention gap?
1. Which measurements justify the chosen in-flight depth?

## Source Grounding

The detailed source inspection is recorded in
`notes/source-reading/2026-07-29-concurrent-retained-fid-reads.md`.

The principal references are:

- 9front's `0intro(5)`, `read(5)`, `flush(5)`, and `clunk(5)` protocol
  manuals for tagged concurrency, positional reads, and cancellation.
- plan9port Acme's `logf.c` and `xfid.c` for retained per-fid event cursors and
  blocked read handling.
- Inferno registry's `Event.queue` for the one-read-per-implicit-cursor rule.
- `knusbaum/go9p` for tagged same-fid calls, stream readers, and
  flush-before-clunk client behavior.
- [`NERVsystems/llm9p`](https://github.com/NERVsystems/llm9p) for a current
  blocking LLM output file with an implicit stream cursor.
- [`Barre/ZeroFS`](https://github.com/Barre/ZeroFS) for a current Rust 9P
  server with explicit flush and clunk barriers.
