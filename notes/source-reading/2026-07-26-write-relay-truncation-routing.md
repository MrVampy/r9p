# Write relay truncation routing

Date: 2026-07-26

## Question

How should an existing front-published write relay behave when a generic 9P
client opens it with `OWRITE | OTRUNC`, especially when the same path is also
under a dynamic backend-relay prefix?

## Sources inspected

- `crates/core/src/mode.rs`
- `crates/core/src/blocking.rs`
- `crates/front/src/front.rs`
- `crates/front/src/model.rs`
- `crates/front/src/tree.rs`
- `crates/front/src/tests.rs`
- `refs/coordinator/src/native/r9p_listener/src/front_feed_relay.rs`
- `refs/coordinator/src/native/r9p_listener/src/front_feed.rs`
- `refs/coordinator/src/native/r9p_listener/src/tests/front_feed_tests.rs`

## Findings

- The blocking client's ordinary replacement write uses `OWRITE | OTRUNC`.
- A front write relay already models a replacement command: it buffers the
  caller's bytes and commits them through its registered relay owner on clunk.
- `open_allowed` rejected `OTRUNC` before checking whether the node was a write
  relay. This made a valid replacement write indistinguishable from an
  unsupported mutation of an ordinary pushed file.
- Coordinator's listener consequently routed every truncating open below a
  backend-relay prefix to the generic backend session. That is correct for an
  ordinary pushed file, but wrong for an exact front-owned write relay.
- The failure is observable during service enrollment: a granted service can
  walk its pending `/srv/<service>` rendezvous, yet the truncating open leaves
  the exact write relay and tries to rematerialize the path through a broader
  `/srv` backend relay.

## Effect

The front contract now admits `OWRITE | OTRUNC` for write relays while keeping
truncation denied for ordinary front files. The Coordinator listener can query
that open capability and prefer the exact write relay over a broader backend
relay. Ordinary backend-relayed truncation remains unchanged.
