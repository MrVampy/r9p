# Remove Clunks Fid Even On Remove Error

Date: 2026-05-13.

## Question

Should `r9p::server` keep a fid live when the backend rejects `Tremove`, or should `Tremove` always consume the fid?

## Files Inspected

- `docs/source-map.md`
- `src/server.rs`
- `src/fid.rs`
- `src/message.rs`
- `refs/plan9port/man/man9/remove.9p`
- `refs/plan9port/src/lib9p/srv.c`
- `refs/plan9port/src/lib9pclient/close.c`

## Findings

The Plan 9 message reference says `remove` asks the server to remove the file represented by `fid` and to clunk that fid even if removal fails. It also frames `remove` as a `clunk` with a conditional remove side effect.

plan9port `lib9p` implements that shape by removing the fid from the server fid pool before permission and backend remove handling in `sremove`. The plan9port client helper `fsfremove` also sends `Tremove` and then releases the local client fid object regardless of the RPC result.

Before this note, `r9p::server::handle_remove` looked up the fid, called `FileTree::remove`, and only removed the fid from the session on backend success. That left a fid live after a failed remove, which contradicted the protocol reference and the server behavior Plan 37 wants Vault to inherit from `r9p`.

## Effect

`r9p::server::handle_remove` now removes the fid after a successful session lookup even when `FileTree::remove` returns an error. Unknown fids still return `EBADFID` without changing session state. The new regression test `remove_clunks_fid_even_when_backend_rejects_remove` pins the behavior.

This closes the small Plan 37 server-parity gap about failed `Tremove` cleanup. It does not address the larger async or split-completion port-process work.

## Open Questions

- The future Vault port process still needs a cancellable or split-completion server path; this note only covers synchronous `Tremove` fid cleanup.
