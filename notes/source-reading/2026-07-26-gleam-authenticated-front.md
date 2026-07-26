# Gleam authenticated front export

Date: 2026-07-26

## Question

Can a Gleam service publish a direct r9p front that enforces the same P9any
Noise IK session authentication advertised by its Coordinator service
descriptor?

## Sources inspected

- `crates/front/src/serve.rs`
  - `Front::serve_tcp`
  - `Front::serve_tcp_authenticated`
- `crates/front/src/abi/mod.rs`
  - `r9p_front_serve_tcp_authenticated`
- `crates/front/tests/conformance.rs`
  - `abi_authenticated_serve_binds_transport_principal_to_attach_uname`
- `crates/beam-port/src/front_port.rs`
  - `FrontManager::handle`
- `bindings/gleam/src/r9p/front.gleam`
  - `serve_tcp`

## Findings

The reusable Rust front and its C ABI already enforce authenticated TCP
exports. Authentication is completed before 9P begins, and the verified peer
principal becomes the only admitted attach username.

The BEAM port and Gleam binding exposed only the unauthenticated
`Front::serve_tcp` path. A Gleam service could advertise an authenticated
authority boundary in a registration descriptor without selecting the
corresponding server mechanism at its own direct endpoint.

## Effect

The BEAM front protocol now has `front-serve-tcp-authenticated`, and the Gleam
binding exposes `serve_tcp_authenticated`. This is a thin binding over existing
generic r9p mechanism. It adds no Coordinator policy or application meaning.

An end-to-end beam-port test generates separate client and server keys, starts
an authenticated front, and reads its projection through an authenticated
namespace client.

## Open questions

None for this binding. Applications still own their server configuration and
Coordinator registration policy.
