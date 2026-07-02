# Plan 83 Control Surface Boundary

Date: 2026-07-02.

Question: how should Plan 83 slice 3 expose local session status, snapshot, stat, list, and read requests without making FUSE or the CLI the session owner?

## Sources Checked

- Vault `docs/plan/83/index.md`: slice 3 asks for a local Unix socket or equivalent IPC for status, read, stat, list, and snapshot requests, with `r9p session status` and `r9p session snapshot` proof against a synthetic server.
- Vault `docs/architecture/61-door-attached-namespace-sessions.md`: the session manager owns the local attachment and access projections while the runtime door remains the backend authority.
- `crates/cli/src/main.rs` and `crates/cli/src/commands/mod.rs`: command dispatch and usage shape.
- `crates/cli/src/target.rs` and `crates/cli/src/transport.rs`: current target/address and Unix-socket dialing conventions.
- `crates/cli/tests/cli_machine.rs`: existing synthetic 9P server fixture and subprocess command proof style.
- `crates/session/src/client.rs` and `crates/session/src/cache.rs`: reusable client, timeout, stat, directory, and freshness mechanisms already extracted in slices 1 and 2.
- `crates/session/src/control/mod.rs`, `request.rs`, `snapshot.rs`, and `json.rs`: local socket request parsing, response routing, snapshot walking, and JSON formatting.
- `crates/cli/src/commands/session.rs` and `crates/cli/tests/session_control.rs`: `r9p session` command shape and the live socket proof.

## Findings

- The local control socket belongs in `crates/session`, not `crates/cli`, because the long-lived attachment is the session owner. The CLI should be a thin client that starts the socket owner or sends requests to it.
- The first CLI surface can be `r9p session serve --socket PATH [endpoint]`, `r9p session status --socket PATH`, `r9p session snapshot --socket PATH [--depth N] PATH`, `r9p session stat --socket PATH [PATH]`, `r9p session list --socket PATH [PATH]`, and `r9p session read --socket PATH PATH`.
- The local Unix socket should serve a small 9P namespace, not a bespoke line protocol. JSON remains the response payload format, while the transport stays normal r9p so existing clients can inspect the session with ordinary `read` operations. Parameterized requests use `/query` as an ORDWR RPC file with JSON request bodies.
- Stat, list, and read can use the same control protocol and the same long-lived 9P attachment. Since the control socket server is long-lived, per-request walked or cloned fids should be clunked even when stat/open/read returns an error.
- Snapshot reports should preserve successful entries when a child branch fails and list the failed branch under a typed `degraded` array. A denied or vanished child is report evidence, not a reason to discard the whole snapshot.
- The session crate uses `serde_json` for `/query` request parsing so the JSON contract is parsed by a real parser instead of a bespoke string scanner. This dependency stays outside `crates/core`.
- This slice should prove status, snapshot, stat, list, and read over a synthetic 9P server.

## Effect

- Add a `session::control` module with Unix-socket 9P serve/request helpers, status response, and snapshot response.
- Keep control request parsing in a focused module so `control/mod.rs` remains the socket coordinator rather than a parser and response monolith.
- Add a `r9p session` CLI command that either starts the local socket owner or talks to it for status, snapshot, stat, list, and read.
- Add a focused CLI integration test with a synthetic 9P server, a session socket process, all current read-only control requests, a direct `r9p -a unix!SOCKET read /status` proof, and a direct `r9p -a unix!SOCKET rpc /query '{"op":"stat","path":"/data"}'` proof.
- Extend the synthetic tree with a denied child and prove snapshot output keeps the visible siblings while returning `reason="denied"` for the degraded branch.
