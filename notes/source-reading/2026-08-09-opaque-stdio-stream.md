# Opaque stdio stream

Date: 2026-08-09.

## Question

Can an unmodified stdio protocol such as MCP use a namespace-native 9P
service without adding protocol-specific semantic translation to r9p?

## Sources inspected

- `crates/cli/src/commands/con.rs`
- `crates/session/src/client/namespace.rs`
- `crates/core/src/server/connection.rs`
- `docs/guides/event-driven-9p.md`
- `notes/source-reading/2026-07-24-native-exec-session-transport.md`
- `notes/source-reading/2026-07-28-offset-cursor-session-survival.md`
- plan9port `src/cmd/9p.c`, especially `xcon` and `rdcon`
- Model Context Protocol draft transport specification
- Codex MCP configuration documentation

## Findings

MCP separates its JSON-RPC message semantics from its transport binding. Its
current transport specification permits custom bidirectional transports and
recommends reusing stdio newline framing over reliable byte streams. Codex and
Claude can both launch an arbitrary stdio command as an MCP server.

The existing `r9p con` already retains read and write fids on one multiplexed
9P connection, which preserves one application-session boundary. It is not a
machine protocol adapter, however: it inherits plan9port's interactive
carriage-return filtering and control-R exit behavior. Its optional resumable
mode may also repeat writes, which is valid only for an explicitly replay-safe
application cursor contract.

## Decision

Add `r9p stream` as a separate machine-facing command over the existing duplex
mechanism. It copies stdin and stdout bytes without inspection or mutation,
uses blocking reads for event-driven output, and never reconnects or replays a
write. Stdin EOF clunks the write fid; output EOF ends the adapter.

The namespace service owns one isolated logical session and its process
lifecycle. It may relay the opaque bytes to an existing MCP stdio process.
r9p does not parse JSON-RPC, negotiate MCP capabilities, select tools, or map
namespace files into MCP operations.

Semantic translation remains necessary only for a separate application facade
that deliberately presents a non-MCP namespace contract as MCP tools.

## Verification

`crates/cli/tests/cli_stream.rs` exercises a cancellable asynchronous stream
server and proves that control-R, carriage return, NUL, non-UTF-8 bytes, and a
multi-frame payload round trip unchanged.
