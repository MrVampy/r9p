# Zero-timeout Resumable Streams

Date: 2026-07-29

## Question

How should `r9p con --resume` represent an intentionally unbounded blocking
read without turning it into an immediate timeout?

## Sources checked

- `crates/cli/src/main.rs`
  - `parse_request_timeout`
- `crates/cli/src/commands/con.rs`
  - `stream_target`
  - `resumable_con`
- `crates/session/src/resumable_fid.rs`
  - `ResumableFid::read`
  - `ResumableFid::write`
  - `ResumableFid::clunk`
  - `open_binding`
- `crates/core/src/multiplex/client.rs`
  - `MultiplexedClient::wait_pending_response_timeout`
- `docs/guides/event-driven-9p.md`
  - blocking reads as the subscription primitive
- `../vault-apps/agents/crates/runner/src/bin/agents-operating-exec-client.rs`
  - the Codex exec-server bridge selects `r9p con --resume` with request
    timeouts disabled

## Finding

The CLI correctly parses `--request-timeout 0` as no deadline. The resumable
stream adapter then collapsed that absence into `Duration::ZERO` and passed it
to the explicitly timed multiplexed operations. Those operations interpret
zero literally and expire immediately.

This broke the event-driven contract: an idle full-duplex stream must block
until output, transport failure, or cancellation. It must not manufacture an
immediate timeout and tear down a healthy application session.

`ResumableFid` already uses zero as the no-deadline sentinel at its public
boundary. It now dispatches that case to the ordinary blocking client
operations for walk, open, read, write, and clunk. Positive durations continue
to use the bounded variants.

## Effect

- `r9p con --resume --request-timeout 0` retains its two fids and waits for
  application output.
- Transport recovery and exact-offset replay remain unchanged.
- A regression runs the existing reconnect and replay proof through the
  no-deadline path as well as the bounded path.
