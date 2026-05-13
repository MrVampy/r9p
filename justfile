set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

# ----------------------------------------------------------------------------
# Agent-loop layering — same four-tier shape used in the sibling projects
# (Racme, r9pfuse) so an agent learns one workflow across the trio.
#
#   Tier 1 (`just check`):   cargo check. Sub-second incremental.
#   Tier 2 (`just lint`):    clippy + fmt-check + machete. ~seconds.
#   Tier 3 (`just verify`):  full gate. lint + nextest + doc + deny. Minutes.
#   Tier 4 (`just audit`):   mutation testing + outdated. Hours; schedule.
# ----------------------------------------------------------------------------

default:
  @just --list

versions:
  @cargo --version
  @rustc --version

# ---- Formatting ----

fmt:
  @cargo fmt --all
  @nixpkgs-fmt flake.nix

fmt-check:
  @cargo fmt --all -- --check
  @nixpkgs-fmt --check flake.nix

# ---- Tier 1: per-iteration ----

check:
  @cargo check --all-targets

# ---- Tier 2: pre-review ----

clippy:
  @cargo clippy --all-targets -- -D warnings

machete:
  @cargo machete

lint: fmt-check clippy machete

# ---- Tier 3: pre-commit gate ----

test:
  @cargo nextest run

doc:
  @cargo doc --no-deps --document-private-items

deny:
  @cargo deny check

verify: lint test doc deny

# ---- Tier 4: background sweeps ----

mutants:
  @cargo mutants

outdated:
  @cargo outdated --root-deps-only

audit: mutants outdated
