# Authenticated referral session endpoints

## Question

What is the smallest shared change needed for source services to remain behind
the logical namespace while callers establish direct, authenticated sessions?

## Sources inspected

- `crates/core/src/export_descriptor.rs`
- `crates/front/src/serve.rs`
- `crates/front/src/abi/mod.rs`
- `crates/front/bindings/deno/front_sink.ts`
- `vault-apps/agents/crates/runner/src/service/config.rs`
- `vault-apps/sensors/reddit-watcher/src/registration.ts`
- `vault-apps/sensors/youtube-watcher/src/registration.ts`
- `vault-apps/sensors/x-watcher/src/registration.py`
- `vault-apps/sources/catalog/src/service.rs`

## Finding

The Rust descriptor contract already has a validated `SessionEndpoint`, and the
front already serves an authenticated TCP endpoint from the same in-process
tree. The Agents service demonstrates the intended composition: publish a
normal service descriptor with a caller-reachable authenticated session
endpoint, then let the caller-local r9p client establish and retain the direct
session after coordinator admission.

The Deno descriptor renderer was the only shared binding without a typed
session endpoint surface. The source services therefore published only their
loopback primary endpoints.

## Design effect

- Add the typed session endpoint to the shared Deno descriptor renderer.
- Let each source service serve its existing tree on a second authenticated
  endpoint and publish that endpoint in its descriptor.
- Keep coordinator responsible for registration, admission, and addressing.
- Keep established source traffic direct between the caller and source service.
