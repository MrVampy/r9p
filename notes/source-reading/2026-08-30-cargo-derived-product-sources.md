# Cargo-derived r9p product sources

Date: 2026-08-30.

## Question

Can FUSE and mount-CLI source changes preserve the general client, Front, and
BEAM product identities without adding an r9p-specific product registry or
dependency map?

## Sources inspected

- `Cargo.toml` and every `crates/*/Cargo.toml` workspace edge.
- `flake.nix` package and check construction.
- `crates/fuse/src/`, the former `crates/cli/src/commands/mount/` tree, and the
  new `crates/mount-cli/` ownership boundary.
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
- r9p initially defeated that constructor by materializing one
  `workspaceSource` from `.cargo`, `Cargo.lock`, `Cargo.toml`, and the complete
  `crates` tree before calling Cargo-Nix. The selected Front derivation thus
  retained evaluator observations of a source object that already contained
  CLI and FUSE content.
- The general `cli` Cargo member also depended directly on `fuse` and owned the
  mount parser, session-mount flags, supervisor, and mount tests. Removing the
  Cargo edge behind a feature would not be enough: `mappedCrateSource` would
  still correctly include those mount-only CLI files in every general client
  build.
- The Front header and Deno bindings and the BEAM Gleam bindings are existing
  non-Rust package inputs. They remain exact file or subtree inputs beside the
  Cargo-derived Rust products.

## Failed live proof

The first FUSE-only source proposal promoted successfully with 15 green r9p
checks, but Dependencies admitted all 41 direct r9p consumers. Only Calendar,
Coordinator, and Credentials cut off immediately. Unrelated trading sensors
and actuators refreshed their r9p locks and rebuilt package checks because
their selected input still observed the broad workspace source.

This is negative Plan 02 evidence. Cargo-derived products were structurally
present, but the source object supplied to the plugin erased the intended
boundary before `mappedCrateSource` could narrow it.

## Effect

`flake.nix` obtains Rust packages and per-member tests from the native Cargo
graph and passes the raw flake root to Cargo-Nix. `mappedCrateSource` remains
the sole local-crate source constructor. The flake introduces no product
manifest, export list, `.depignore`, consumer mapping, or r9p-specific
Dependencies rule.

The general `cli` member now has no FUSE dependency and owns only a stable raw
dispatch seam. The new `mount-cli` member owns `r9p-mount`, every mount-specific
argument and test, and the FUSE dependency. Its typed `MountAdapter` enters the
same general command graph and starts session-hosted FUSE against the same
in-process `ControlRuntime`.

The evaluator regression constructs separate sources with one FUSE file and
one mount-CLI file removed. Both edits must preserve the general client's
mapped source and package derivation while changing the `with-mount` product.
The Front source and package must also remain identical across both edits.
Content checks require the default package to contain only `r9p`, the helper
package to contain only `r9p-mount`, and `with-mount` to contain both.

## Proposed package and host seam

- `packages.default` and `packages.r9p` are the stable general client. M7,
  NucBox, WSL, Tablet, and service consumers continue selecting this product
  while they have no declared FUSE mount.
- `packages.mount-helper` contains only the mount adapter and is primarily a
  construction and regression surface.
- `packages.with-mount` combines the general client and helper, binding the
  exact helper path into the `r9p` launcher. Tuxedo selects this product for the
  Newsgroups download mount, Coordinator namespace mount, and interactive
  `r9p mount` use.
- A future thin client selects `with-mount` only when its host composition adds
  a real mount consumer. Merely being a thin client does not add FUSE bytes.

## Remaining live proof

After this correction is published, separate FUSE-only and mount-CLI-only
source changes must prove that ordinary client and Front consumers no longer
enter validation while Tuxedo's `with-mount` selection advances. Missing or
incomplete evaluator evidence must continue to fail closed.
