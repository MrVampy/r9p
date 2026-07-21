# System mount supervisor scope

Date: 2026-07-21

## Question

How should `r9p mount status`, `ensure`, and `stop` address a persistent system
unit without confusing it with a same-named user unit?

## Sources inspected

- `crates/cli/src/commands/mount/supervisor.rs`
- `crates/cli/src/commands/mount/tests.rs`
- Vault `docs/operations/runtime-lifecycle-and-9p.md`
- Vault `docs/operations/plan9port-client.md`
- Vault host `hosts/tuxedo/vault-namespace.nix`
- The generated Tuxedo `vault-live-r9p-mount.service`

## Findings

- The supervisor accepted a unit name while hard-coding `--user` for every
  `systemctl` and `systemd-run` invocation.
- The canonical Vault mount is now a declarative system unit that runs the
  FUSE client as the unprivileged operator account.
- A user and system manager may each own the same unit name. Automatically
  searching both scopes would make status ambiguous and could cause ensure or
  stop to act on a different owner than the one inspected.

## Effect

The supervisor contract now requires `--unit-scope user|system` whenever
`--unit` is supplied. The typed scope is shared by status, ensure, and stop, so
unit inspection and lifecycle commands address one explicit systemd manager.
Vault's persistent mount uses `--unit-scope system` in its operator proof.

## Open questions

- None for this scope correction.
