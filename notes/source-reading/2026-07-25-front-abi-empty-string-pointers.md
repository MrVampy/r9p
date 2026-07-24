# Front ABI empty string pointers

Date: 2026-07-25

## Question

Why did the Deno front return `INVALID` without error detail when a resolved
namespace client used a local Unix resolver and no resolver session-auth
configuration?

## Sources inspected

- `crates/front/bindings/deno/front_sink.ts`
  - `FrontHost.resolvedRpc`
  - `FrontHost.decodeResolvedResponse`
- `crates/front/src/abi/mod.rs`
  - `str_arg`
  - `bytes_arg`
- `crates/front/src/abi/client.rs`
  - `r9p_front_client_resolved_rpc`
  - `resolved_rpc`
- `crates/front/include/r9p_front.h`

## Findings

The Deno binding represents an empty optional resolver-auth path as a
zero-length FFI buffer. Deno may pass that buffer as a null pointer with length
zero. `bytes_arg` already accepted this canonical C representation, but
`str_arg` rejected every null pointer regardless of length. The resolved client
therefore returned the ABI `INVALID` status before it could record an error.

The public ABI explicitly permits an empty resolver-auth string to select a
contained unauthenticated transport. A null pointer with length zero must
therefore decode as the empty string, while a null pointer with nonzero length
remains invalid.

## Effect

`str_arg` now follows the same zero-length rule as `bytes_arg`. The header
documents the rule, and a unit regression covers both the admitted empty case
and the rejected nonempty case. This is a generic front ABI correction; source
services do not need transport-specific workarounds.

## Open questions

None.
