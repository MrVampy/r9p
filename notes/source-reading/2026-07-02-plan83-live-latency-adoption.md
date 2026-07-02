# Plan 83 Live Latency And Adoption Gate

Date: 2026-07-02.

Question: after session-hosted FUSE and shared cache fixes, is the door-attached session manager fast enough to serve as the agent query surface, and should the live `.vault/live` mount be replaced now?

## Sources Checked

- Vault `docs/plan/83/index.md`: slice 7 requires a live latency and reliability proof; Claude Amendment 2 requires an explicit `.vault/live` adoption decision after the matrix.
- `crates/cli/tests/session_live_latency.rs`: host-gated typed latency matrix added in this slice.
- `crates/session/src/cache.rs`: directory cache now seeds child stat entries, which is necessary for warm recursive snapshots to avoid per-child `Tstat` calls.
- Live M7 door at `192.168.0.30:9564`.
- Current live FUSE mount at `/home/mrvamp/Dropbox/Projects/Vault/.vault/live`, whose mount status reports `change_feed: connected`.

## Findings

- The session control namespace is a useful agent query surface now. Warm `/srv` list and depth-2 snapshot both answer in the low single-digit millisecond range from a live M7 session.
- Cold recursive snapshot remains server-evaluation-bound. `/srv` depth 2 was about 0.62 seconds cold in this sample, then about 0.004 seconds warm.
- Current standalone FUSE is already good for warm ordinary filesystem navigation. Warm root and `/srv` readdir were about 0.0001 seconds in the sample.
- Stopping the temporary session daemon did not affect raw door access: a fresh raw `ls /srv` succeeded after session stop in about 0.067 seconds.

## Live Matrix

Endpoint: `192.168.0.30:9564`

Current FUSE mount: `/home/mrvamp/Dropbox/Projects/Vault/.vault/live`

Command:

```bash
R9P_PLAN83_ENDPOINT=192.168.0.30:9564 \
R9P_PLAN83_FUSE_MOUNT=/home/mrvamp/Dropbox/Projects/Vault/.vault/live \
cargo test -p cli --test session_live_latency -- --ignored --test-threads=1 --nocapture
```

Observed rows:

```text
plan83_latency	label=raw_cli_ls_root	seconds=0.072658	bytes=300
plan83_latency	label=raw_cli_ls_srv	seconds=0.078130	bytes=100
plan83_latency	label=raw_cli_read_status	seconds=0.070566	bytes=1419
plan83_latency	label=session_status	seconds=0.022874	bytes=575
plan83_latency	label=session_control_rpc_stat_status	seconds=0.024680	bytes=414
plan83_latency	label=session_list_srv_cold	seconds=0.067189	bytes=2046
plan83_latency	label=session_list_srv_warm	seconds=0.002173	bytes=2046
plan83_latency	label=session_snapshot_srv_depth2_cold	seconds=0.617301	bytes=5649
plan83_latency	label=session_snapshot_srv_depth2_warm	seconds=0.003652	bytes=5649
plan83_latency	label=session_read_status	seconds=0.105560	bytes=3049
plan83_latency	label=fuse_root_readdir	seconds=0.035514	bytes=299
plan83_latency	label=fuse_root_readdir_warm	seconds=0.000095	bytes=299
plan83_latency	label=fuse_srv_readdir	seconds=0.062305	bytes=99
plan83_latency	label=fuse_srv_readdir_warm	seconds=0.000095	bytes=99
plan83_latency	label=fuse_read_status	seconds=0.056069	bytes=1419
plan83_latency	label=raw_cli_ls_srv_after_session_stop	seconds=0.067145	bytes=100
```

## Adoption Decision

Do not replace the live `.vault/live` mount yet.

The reason is not performance. The live numbers show the session query surface is fast when warm, and current standalone FUSE is also fast for warmed filesystem navigation. The reason is lifecycle correctness: `.vault/live` is an existing operator mount, while `r9p session serve --mount` is proven as a hosted projection but not yet installed as the user's lifecycle-managed mount unit. Replacing the live mount inside this slice would mix Plan 83 measurement with host adoption mechanics and would disturb the operator surface without a separate rollback-tested unit change.

The intended near-term posture is:

- Keep `.vault/live` on the standalone mount as the stable Linux filesystem projection.
- Use `r9p session serve` as the parallel agent query/control surface for snapshots, freshness, and diagnostics.
- Adopt session-hosted FUSE only after a dedicated lifecycle slice defines how the user service is started, stopped, statused, and rolled back to standalone `r9p mount`.

## Proof

- `cargo test -p cli --test session_live_latency -- --ignored --test-threads=1 --nocapture` without env skips cleanly.
- `R9P_PLAN83_ENDPOINT=192.168.0.30:9564 R9P_PLAN83_FUSE_MOUNT=/home/mrvamp/Dropbox/Projects/Vault/.vault/live cargo test -p cli --test session_live_latency -- --ignored --test-threads=1 --nocapture` passes and emits the matrix above.

## Open Questions

- Cold recursive snapshot still needs the Plan 83 slice-8 decision ladder: client-side discovery concurrency first, then front materialization coverage, then only if still necessary a generic runtime snapshot primitive.
- The Vault Plan 83 document should be updated from this note in a clean Vault-doc slice. The Vault checkout was dirty with unrelated work while this r9p slice landed, so this run did not edit the referenced plan file directly.
