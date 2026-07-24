---
title: Governed direct namespace-path resolution
date: 2026-07-24
---

# Governed direct namespace-path resolution

## Question

How should a client reach a registered service without making the Coordinator a
9P byte relay?

## Sources inspected

- `crates/core/src/connection_descriptor.rs`
- `crates/session/src/resolved.rs`
- `crates/session/src/client.rs`
- `crates/session/src/opened_fid.rs`
- `crates/front/src/abi/client.rs`
- `crates/beam-port/src/lib.rs`
- `bindings/gleam/src/r9p.gleam`
- Coordinator `src/core/api/namespace/mutation/connection_server.gleam`
- Coordinator `src/core/namespace/service/registry.gleam`
- Coordinator `src/core/namespace/service/registry/render.gleam`

## Findings

- `/cs` already performs finite-lived service endpoint resolution and carries
  the resolving principal to endpoints that request it.
- `/srv` registrations already declare logical `namespace_mount_paths`.
- `r9p-session` already owns direct endpoint connection, portable authority
  bindings, and longest-prefix client-side namespace composition.
- The missing fact for resolving an arbitrary logical path was the selected
  mount path. Without it, a client cannot rebase the logical suffix onto the
  service's `exported_root`.
- Coordinator registry admission prevents duplicate exact mount claims, while
  nested mounts are valid. Path resolution must therefore select the longest
  registered prefix.

## Effect

`r9p-connection.v1` now optionally carries `namespace_mount_path` for
path-based resolution. A client may write either a service name or an absolute
namespace path to `/cs`; the latter resolves the longest registered mount.
`r9p-session` rebases the requested path locally and connects directly to the
service. The front C ABI and BEAM adapter expose this as ordinary resolved
read/RPC/list/stat operations.

`ResolvedPath` also owns the generic one-shot direct read and same-fid RPC
lifecycle. Those helpers connect to the resolved service, enforce bounded
timeouts and response size, reject a short request write, and close only the
direct service connection. Language bindings and Rust applications therefore
share the same transport-neutral operation rather than reproducing that
lifecycle locally.

Coordinator remains responsible for registration, policy, and address
selection. It does not carry the service operation's 9P frames.

## Open questions

- Sensitive services should publish an authenticated session endpoint and bind
  each admitted principal to distinct client key material. Loopback containment
  alone does not provide per-service principal authenticity.
- Long-lived consumers may later cache resolved generations until their finite
  validity expires. Initial one-shot operations intentionally resolve before
  each call.
