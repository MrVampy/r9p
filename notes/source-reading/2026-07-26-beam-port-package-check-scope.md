# Beam port package check scope

## Question

Why does a downstream build of the `r9p-beam-port` package compile and test the
entire r9p workspace?

## Sources inspected

- `flake.nix`, specifically the `beamPort`, `frontTests`, `packages`, and
  `checks` definitions.
- `Cargo.toml`, specifically the workspace members.
- Nixpkgs `cargo-check-hook.sh`, specifically the flag assembly from
  `cargoTestFlags`.

## Findings

The `beamPort` derivation already limits its build to `-p beam-port`, but it
leaves the default Rust package check phase enabled. With no
`cargoTestFlags`, that phase checks and tests the whole Cargo workspace. This
made a downstream artifact build compile unrelated CLI, FUSE, reverse, and
session targets and run their tests.

The existing `frontTests` derivation already carries the focused
`cargoTestFlags` for `front` and `beam-port`. It was exposed as a package but
not as a flake check.

## Effect

The `beam-port` artifact now disables its in-package check phase. The existing
focused `frontTests` derivation is exposed as `checks.beam-front`, so r9p's own
flake check retains the relevant test gate while downstream consumers build
only the artifact they requested.

## Open questions

The default `r9p` CLI package still runs its check phase during an artifact
build. If downstream timing shows the same problem there, its tests should be
moved to an explicit flake check by the same separation.
