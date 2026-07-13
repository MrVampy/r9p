# FUSE Read Handle Replay After 9P Reconnect

Date: 2026-05-29

Question: why can a read through `r9p mount` surface `Stale file handle` after a 9P reconnect, and where should the repair live?

Files and functions inspected:

- `crates/fuse/src/fuse/ops/io.rs`: `read`, `open`, `release`
- `crates/fuse/src/fuse/mod.rs`: `bound_node_fid`, `reconnect`, `refresh_node`
- `crates/fuse/src/node.rs`: `NodeTable`, `Handle`, path-backed node rebind state
- `crates/cli/src/commands/serve.rs`: `export_with_config`, `serve_connection`

Findings:

- `r9p mount` already rebinds path-backed nodes after a reconnect or namespace refresh, but the open FUSE file handle still carried the old opened 9P fid.
- The old `read` path returned `ESTALE` to Linux after a transport error or stale namespace fid. That is correct for non-replayable handles, but too harsh for read-only file handles because POSIX consumers such as Git abort immediately on `ESTALE`.
- A read-only file handle can be replayed by rebinding the node path, cloning a fresh fid, opening it `OREAD`, replacing the handle's opened fid, and retrying the same offset and size once.

Effect on code:

- Keep the repair in `crates/fuse`, not in `crates/core`; this is Linux FUSE handle semantics over a generic 9P client.
- Read-only file handles are now replayable after reconnect or namespace refresh.
- Directory handles and write-on-release handles still fail closed with `ESTALE`; replaying them could duplicate close-commit or mutate semantics.
