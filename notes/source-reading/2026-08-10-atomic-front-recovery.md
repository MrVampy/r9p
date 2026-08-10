# Atomic Front recovery publication

Date: 2026-08-10

## Question

How can a restarted publisher reconcile a retained r9p Front without briefly
removing valid paths, while keeping service policy and lease recovery outside
r9p?

## Sources inspected

- `crates/front/src/front.rs`, especially `Front::set_pushed_file`,
  `Front::set_pushed_directory`, and `Front::remove_subtree_if_exists`
- `crates/front/src/model.rs`, especially `State::place_pushed_file`,
  `State::place_pushed_directory`, `State::remove_subtree_if_exists`, and
  `State::remove_node_recursive`
- `crates/front/src/tree.rs`, especially retained Front reads and relay-backed
  mutations
- `crates/front/src/tests.rs`, especially subtree removal and principal-root
  tests
- Coordinator `src/core/api/r9p/front_feed_publisher.gleam`, especially
  `publish_current` and `clear_full_publish_subtrees`
- Coordinator `src/native/r9p_listener/src/front_feed.rs`, especially the
  standing Front control operations
- Coordinator `src/native/r9p_listener/src/namespace_referrals.rs`, especially
  retained referral replacement and lease expiry
- Coordinator `src/core/runtime/actor/startup.gleam`, especially startup from
  an empty in-process service registry

## Findings

- The standing Rust listener retains a valid Front across a BEAM restart, but
  the publisher previously cleared `views/runtime` before writing its new
  image. Fresh namespace walks therefore observed `ENOENT` even though the
  retained listener and its sockets never stopped.
- Writing the current image before removing stale paths prevents the empty
  interval, but only if stale removal is one atomic state transition. Repeated
  `remove_subtree_if_exists` calls expose partial reconciliation and wake
  blocked readers once per removal.
- The required reusable mechanism is a Front operation that validates a
  retained path set, preserves all required ancestors, and removes maximal
  stale subtrees while holding the existing Front state mutex.
- The operation is useful to any publisher-owned mutable Front. It does not
  need to know about services, admission, leases, Coordinator, or provider
  recovery, so it belongs in `crates/front`.
- Service-referral merge policy is not generic Front state. Coordinator must
  decide when retained referrals remain admissible and when a fully recovered
  registry is authoritative.
- Retained Front principal bindings must not point at removed node IDs. Atomic
  pruning can drop bindings whose roots no longer exist, while the publisher
  separately retains the admitted current principals.

## Effect on design

- Add `Front::retain_subtree_paths` as a failure-atomic, single-wakeup pruning
  operation.
- A publisher first writes or updates every current node, then invokes the
  retain operation with the complete desired path set.
- Coordinator recovery initially merges lease-bound referrals into the
  standing listener instead of replacing them from an empty process-local
  registry.
- Coordinator performs one event-driven authoritative reconciliation after
  the previous maximum ready-lease interval. It does not poll for convergence.

## Open questions

- Whether a future batch publication transaction should combine node writes
  and retention in one Front operation. The current write-then-prune order is
  continuity-safe, but readers can briefly see both old and new siblings.
- Whether relay registration sets should gain their own atomic retain
  operation if a generic publisher starts removing relay paths independently
  of their backing nodes.
