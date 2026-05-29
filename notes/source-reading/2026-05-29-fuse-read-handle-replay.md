# FUSE Read Handle Replay After 9P Reconnect

Date: 2026-05-29

Question: why did Git reading an exported bundle through `r9p mount` surface `Stale file handle`, and where should the repair live?

Files and functions inspected:

- `crates/fuse/src/fuse/ops/io.rs`: `read`, `open`, `release`
- `crates/fuse/src/fuse/mod.rs`: `bound_node_fid`, `reconnect`, `refresh_node`
- `crates/fuse/src/node.rs`: `NodeTable`, `Handle`, path-backed node rebind state
- `crates/cli/src/commands/git_export.rs`: `git_export_cmd`, `prepare_git_export_source`
- `crates/cli/src/commands/serve.rs`: `export_with_config`, `serve_connection`
- Vault `src/candidate_provider/src/materialization.rs`: `materialize_git_source_bundle`

Findings:

- Vault's candidate provider mounts an `r9p export git` source and asks `git fetch` to read the exported bundle through the FUSE mount.
- `r9p export git` prepares a clean detached worktree, writes a Git bundle into that worktree, and serves the worktree over ordinary 9P.
- `r9p mount` already rebinds path-backed nodes after a reconnect or namespace refresh, but the open FUSE file handle still carried the old opened 9P fid.
- The old `read` path returned `ESTALE` to Linux after a transport error or stale namespace fid. That is correct for non-replayable handles, but too harsh for read-only file handles because POSIX consumers such as Git abort immediately on `ESTALE`.
- A read-only file handle can be replayed by rebinding the node path, cloning a fresh fid, opening it `OREAD`, replacing the handle's opened fid, and retrying the same offset and size once.

Effect on code:

- Keep the repair in `crates/fuse`, not in `crates/core`; this is Linux FUSE handle semantics over a generic 9P client.
- Read-only file handles are now replayable after reconnect or namespace refresh.
- Directory handles and write-on-release handles still fail closed with `ESTALE`; replaying them could duplicate close-commit or mutate semantics.

Open questions:

- The source mount used by Vault's candidate provider may also want a longer explicit read timeout for large bundle reads, but that is a provider mount-policy choice layered above this FUSE handle repair.
