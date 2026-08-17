# Generated CLI help

## Sources inspected

- `crates/cli/src/main.rs` for global option parsing, command dispatch, and the
  former top-level `usage()` output.
- `crates/cli/src/commands/` for every public command's positional arguments,
  options, nested operations, and machine-mode restrictions.
- `crates/cli/src/target.rs` for the distinction between explicit endpoints and
  `$NAMESPACE` service paths.
- `notes/source-reading/2026-05-13-r9p-cli-plan9port-parity.md` for the prior
  plan9port CLI comparison. The optional plan9port reference checkout is not
  present in this worktree.

## Findings

The public CLI contract was distributed across top-level dispatch, several
command-local parsers, and multiple handwritten usage functions. `r9p --help`
therefore failed as an unknown option, while the command list and detailed
syntax could drift independently.

The protocol and session implementations do not need to know how help is
rendered. A typed parser belongs only in the `cli` crate. Existing command
handlers can retain their semantic validation while receiving normalized
arguments from that parser.

## Decision

Use one Clap derive tree as the public command definition, parser, dispatch
list, and help source. Support `help`, `-h`, `--help`, and nested command help
without adding Clap to any reusable r9p library crate. Retain plan9port flag
clusters and existing command spellings, aliases, namespace resolution, and
machine-mode behavior.
