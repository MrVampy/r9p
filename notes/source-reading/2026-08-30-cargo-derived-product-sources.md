# Cargo-derived r9p product sources

Date: 2026-08-30.

## Question

Can a FUSE-only r9p source change preserve unrelated Front and BEAM product
identities without adding an r9p-specific product registry or dependency map?

## Sources inspected

- `Cargo.toml` and every `crates/*/Cargo.toml` workspace edge.
- `flake.nix` package and check construction.
- `crates/fuse/src/` and `crates/cli/src/commands/mount/` ownership boundaries.
- Cargo-Nix `lib/default.nix`, `lib/mapped-source.nix`, and
  `tests/mapped-source-test.nix` at its automatic local-crate source boundary.

## Findings

- Cargo already defines every Rust member and dependency edge required to
  distinguish the CLI/FUSE, Front, and BEAM products.
- The old Nix packaging discarded that graph by passing one repository-wide
  `rustSource` to three independent `buildRustPackage` calls.
- Cargo-Nix can build the existing workspace-member outputs directly. Its
  shared local-source constructor selects one member subtree and the ancestor
  `Cargo.toml` files needed for workspace inheritance, while dependency
  derivations follow Cargo's resolved edges.
- The Front header and Deno bindings and the BEAM Gleam bindings are existing
  non-Rust package inputs. They remain exact file or subtree inputs beside the
  Cargo-derived Rust products.

## Effect

`flake.nix` now obtains Rust packages and per-member tests from the native Cargo
graph. It introduces no product manifest, export list, `.depignore`, consumer
mapping, or r9p-specific Dependencies rule. A future FUSE/cache change should
alter the FUSE and CLI dependency path while preserving unrelated Front and
BEAM derivations.

## Open proof

After the exact evaluator and r9p packaging cutovers are published, the first
real FUSE-only change must prove that unrelated consumers do not enter the
fan-out. Missing or incomplete evaluator evidence must continue to fail closed.
