# Front ABI Capability Contract

Date: 2026-07-19.

## Question

How should front consumers distinguish an incompatible C ABI change from an
additive front feature without carrying a growing exact-version allowlist?

## Sources Inspected

- `crates/front/include/r9p_front.h`: current C signatures and lifetime rules.
- `crates/front/src/abi/mod.rs`: exported ABI version and function surface.
- `crates/front/bindings/deno/front_sink.ts`: dynamic-library admission.
- Commit `a1c402a` (`front: expose atomic create and write`): ABI 17 to 18.
- Commit `c41adc8` (`front: relay synthetic file reads`): ABI 18 to 19.
- `../vault-apps/sensors/x-watcher/src/front_ffi.py`: Python consumer admission
  and its atomic create-and-write requirement.

## Findings

ABI versions 18 and 19 each added functions without changing an existing C
signature or lifetime rule. Consumers nevertheless had to treat every additive
version as a new exact contract. A bounded version range can admit the known
additions, but it cannot state which feature the consumer actually needs and it
still rejects a future additive release for reasons unrelated to that consumer.

The C boundary already has a safe admission point before handle allocation:
`r9p_front_abi_version()`. A second handle-free query can expose feature bits
without changing the ownership model or any existing operation.

## Decision

ABI generation 20 establishes the current signatures and lifetime rules as one
breaking-change boundary. Consumers require generation 20 exactly, then call
`r9p_front_capabilities()` and require only the named bits for the surfaces they
use.

The initial stable bits cover pushed namespace metadata, request context v2,
synthetic read relay, native client mutations, atomic create-and-write, and
namespace mutation relays. Additive features allocate new bits without changing
the ABI generation. Published bits are never reused, and consumers ignore bits
they do not understand. An incompatible signature, lifetime rule, or existing
data contract advances the ABI generation.

The installed front package now includes `include/r9p_front.h`, so compile-time
constants and the runtime queries come from the same revision as
`lib/libfront.so`.

## Open Questions

None for the current in-process and dynamic-library consumers.
