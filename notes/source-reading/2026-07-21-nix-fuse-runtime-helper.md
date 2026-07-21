# Nix FUSE runtime helper ownership

Date: 2026-07-21

## Question

Where should the `fusermount3` runtime dependency live so `r9p mount` works
from a declarative system service without relying on an ambient user PATH?

## Sources inspected

- `crates/fuse/src/fuse/mount.rs`
- `flake.nix`
- Vault host unit `hosts/tuxedo/vault-namespace.nix`
- The generated Tuxedo `vault-live-r9p-mount.service`

## Findings

- `mount_fuse_attempt` forks and `child_exec_fusermount` resolves
  `fusermount3`, then `fusermount`, with `execlp`.
- The r9p Nix package previously installed the binary without carrying either
  helper in its runtime PATH.
- An interactive user environment happened to expose `fusermount3`, while the
  generated system service had a deliberately narrow PATH. The client reached
  and authenticated the 9P server, then failed locally because the helper
  process could not return a FUSE file descriptor.
- Adding the helper only to one host unit would preserve an undeclared package
  dependency and force every future caller to rediscover it.

## Effect

The default r9p Nix package wraps `r9p` with the `fuse3` binary directory in
its PATH. A flake check asserts that the installed executable retains this
runtime edge. Host services can therefore invoke the package directly without
mount-specific PATH composition.

## Open questions

- None for this packaging slice.
