# BEAM Port Adapter Source Reading

## Date

2026-07-08

## Question

Where should the reusable Gleam/BEAM 9P client adapter live, and what exact surface should it expose so Vault and vault apps can use r9p as the namespace hand without copying Rust client code into each app?

## Files Inspected

- `docs/source-map.md`
- `crates/core/src/blocking.rs`
- `crates/cli/src/commands/machine.rs`
- `crates/front/include/r9p_front.h`
- `/home/mrvamp/Dropbox/Projects/Vault/src/native/r9p_listener/src/bin/vault-r9p-peer-client.rs`
- `/home/mrvamp/Dropbox/Projects/Vault/src/native/r9p_listener/src/peer_client.rs`
- `/home/mrvamp/Dropbox/Projects/Vault/src/core/runtime_r9p_peer_client_ffi.erl`
- `/home/mrvamp/Dropbox/Projects/Vault/src/core/api/r9p/peer_client.gleam`
- `/home/mrvamp/Dropbox/Projects/Vault/src/core/api/r9p/peer_client_port.gleam`
- `/home/mrvamp/Dropbox/Projects/Vault/src/core/api/r9p/client/output.gleam`
- `/home/mrvamp/Dropbox/Projects/Vault/src/core/api/r9p/client/rpc.gleam`

## Findings

- `docs/source-map.md` keeps `crates/core` runtime-neutral and allows runtime adapters only above the reusable protocol core. A BEAM port process belongs outside `crates/core`.
- `crates/core/src/blocking.rs` already has the generic client operations the adapter needs: connect with explicit `msize`, stat, list, read, ranged read, write, and same-fid RPC. No Vault policy is required for these operations.
- `crates/cli/src/commands/machine.rs` defines a stable tab-separated machine output shape for stat and RPC responses. The adapter can reuse that shape while wrapping stdin/stdout responses in hex so BEAM does not parse raw control bytes.
- `crates/front/include/r9p_front.h` ABI v15 exposes outbound client helpers only for read and RPC. That C ABI remains useful for Deno, Python, and other C-ABI consumers, but it is not the right complete surface for BEAM apps that need write/list/stat/ranged-read as well.
- Vault already carries a duplicated Rust peer client with a persistent process, per-target client cache, hex fields, stat/list/read/read-range/write/rpc commands, and a Gleam/Erlang port wrapper. The only Vault-specific part is the fixed default `msize` and local binary resolution.
- Vault's Gleam parsers expect the machine stat, read, write, and RPC output shapes. The reusable adapter should preserve those shapes and add explicit `msize` to the target so callers choose the negotiation size instead of inheriting Vault's historical 8192 default.

## Effect

The implementation adds a new `crates/beam-port` runtime adapter crate and a `bindings/gleam` package. The Rust binary is only the 9P namespace hand: it maintains 9P client connections and performs stat/list/read/read-range/write/rpc. The Gleam package owns the typed caller-facing API and parsing.

The flake now exposes `.#beam-port`, `.#beam-gleam`, and `.#beam` so Vault and vault apps can consume the shared adapter instead of copying peer-client Rust into each repository.

## Open Questions

- Vault still needs to cut over from its local peer client copy to the packaged adapter, then delete the duplicated local peer-client Rust/Erlang/Gleam port code.
- Notification/subscription is not proven by this adapter. Polling and watching cadence remain Gleam-owned; future notify support must be proven from r9p source before exposing it.
