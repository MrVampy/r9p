# Offset-cursor session survival

Date: 2026-07-28.

## Question

Which part of a long-lived full-duplex 9P file can reconnect generically
without making an application write unsafe to repeat?

## Sources inspected

- `crates/core/src/message.rs`
- `crates/core/src/server/types.rs`
- `crates/core/src/multiplex/client.rs`
- `crates/session/src/client_session.rs`
- `crates/session/src/opened_fid.rs`
- `crates/cli/src/commands/con.rs`
- 9front `sys/src/cmd/aan.c`
  (`https://github.com/9front/9front/blob/front/sys/src/cmd/aan.c`)
- Agents `crates/runner/src/operating_service/execution.rs`
- Agents `crates/runner/src/operating_service/handler.rs`

## Findings

Every 9P read and write already carries a 64-bit offset. A stream file can use
that field as an application cursor instead of treating it as an ignored byte
position. The client can then reopen the same path after a new attachment and
repeat the same request without inventing another wire protocol.

That recovery is safe only when the server contract is explicit:

- a repeated write at the last committed input offset must return the original
  success without applying the bytes again;
- a repeated read at the last delivered output offset must return the same
  retained bytes; and
- advancing the read offset acknowledges earlier output and permits bounded
  retention.

This is the numbered-and-replayed shape used by AAN, expressed through native
9P offsets. It does not make ordinary mutable files replayable.

## Effect

The session crate exposes `ResumableFid`, and `r9p con --resume` opts into it.
Both rebuild their fid after a definitive transport failure and repeat the
operation at the exact same offset. Consumers must select this mode only for a
server file that implements the cursor contract.

The server-side process, retention, authorization, and logical-session identity
remain application-owned.

## Open questions

A future generic server helper can package the bounded replay buffer and
deduplicated input cursor once a second backend needs that state machine.
