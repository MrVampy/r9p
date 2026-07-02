# Plan 83 Control Surface Boundary

Date: 2026-07-02.

Question: how should Plan 83 slice 3 expose local session status and snapshot requests without making FUSE or the CLI the session owner?

## Sources Checked

- Vault `docs/plan/83/index.md`: slice 3 asks for a local Unix socket or equivalent IPC for status, read, stat, list, and snapshot requests, with `r9p session status` and `r9p session snapshot` proof against a synthetic server.
- Vault `docs/architecture/61-door-attached-namespace-sessions.md`: the session manager owns the local attachment and access projections while the runtime door remains the backend authority.
- `crates/cli/src/main.rs` and `crates/cli/src/commands/mod.rs`: command dispatch and usage shape.
- `crates/cli/src/target.rs` and `crates/cli/src/transport.rs`: current target/address and Unix-socket dialing conventions.
- `crates/cli/tests/cli_machine.rs`: existing synthetic 9P server fixture and subprocess command proof style.
- `crates/session/src/client.rs` and `crates/session/src/cache.rs`: reusable client, timeout, stat, directory, and freshness mechanisms already extracted in slices 1 and 2.

## Findings

- The local control socket belongs in `crates/session`, not `crates/cli`, because the long-lived attachment is the session owner. The CLI should be a thin client that starts the socket owner or sends requests to it.
- The first CLI surface can be `r9p session serve --socket PATH [endpoint]`, `r9p session status --socket PATH`, and `r9p session snapshot --socket PATH [--depth N] PATH`.
- A line-oriented request with JSON responses is enough for v1. It avoids adding a serialization dependency while still returning typed machine-readable responses.
- This slice should prove status and snapshot over a synthetic 9P server. Read/stat/list can be handled by the same control protocol later without changing the socket ownership boundary.

## Effect

- Add a `session::control` module with Unix-socket serve/request helpers, status response, and snapshot response.
- Add a `r9p session` CLI command that either starts the local socket owner or talks to it.
- Add a focused CLI integration test with a synthetic 9P server, a session socket process, and status/snapshot requests.
