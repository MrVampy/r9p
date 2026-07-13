# Maintained Publication Self-Wake

Date: 2026-07-13.

## Question

Why did the live M7 runtime and standing listener consume roughly 14 CPU cores
while all registered services were healthy and no Nix build was active?

## Sources Inspected

- `crates/core/src/srv_publish.rs`: `maintain_r9p_export`, `maintain_loop`,
  `wait_for_srv_change`, and `publish_with_client`.
- `crates/front/src/abi/publication.rs`:
  `r9p_front_maintain_r9p_export` and maintainer ownership.
- `crates/front/bindings/deno/front_sink.ts`: the Deno
  `maintainR9pExport` call boundary.
- Live M7 namespace paths `/srv/wait/bybit/demo/actuator/state` and its
  advertised `changed-after` path.

## Findings

`maintain_r9p_export` performed a synchronous initial publication and then
spawned `maintain_loop`, whose first action was another publication. A matching
ready registration writes a keepalive. The background thread therefore
advanced the same state token it was about to watch. On M7, each
`changed-after` read returned immediately as changed, the loop wrote another
keepalive, and the cycle repeated.

The live evidence was direct: ten reads of the Bybit actuator state at 200 ms
intervals returned ten different tokens. Stopping that one actuator made the
token remain identical across five reads. Restarting it resumed token churn.
Across the fleet this produced about 13,700 loopback `TIME_WAIT` sockets and
sustained runtime/listener CPU saturation.

## Effect On Code

The maintainer now starts in the wait state after its synchronous publication.
External change reconciliation observes an already-ready registration without
renewing it. Keepalive writes are reserved for the 60-second renewal path and
for recovery after a failed wait. A regression test checks that the initial
keepalive remains current instead of being immediately overwritten by a second
publication.

## Open Questions

None. The remote test and live M7 adoption still need to prove that lease
renewal, missing-registration recreation, and resource usage all remain
correct.
