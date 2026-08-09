# Shared Session Key Group Access

## Question

How should two differently privileged local services use one certified r9p
identity without granting the root service discretionary-access override
capabilities or copying the private key?

## Sources inspected

- `nix/session-auth.nix`: key ownership, directory creation, tmpfiles rules,
  and the key-generation service.
- `flake.nix`: the existing session-auth NixOS module fixture.
- `crates/auth/src/key.rs`: `provision_key_pair`, `write_key_pair`, and
  `write_new_file` key creation and permission behavior.
- `crates/cli/src/commands/auth_keygen.rs`: the secure-default key-generation
  command used by the module.
- `hosts/tuxedo/agent.nix` in the fleet flake: the Agent runtime and hardened
  host supervisor intentionally use one certified service identity.

## Findings

- The module already models a key owner, group, and directory mode, but it
  hard-coded every private key to mode `0600`.
- `auth-keygen` correctly creates a new private key as `0600`, independent of
  its process umask. A tmpfiles adjustment alone therefore does not establish
  a configured `0640` mode on first provisioning; the module must converge the
  declared mode after the generator succeeds.
- The Tuxedo Agent runtime runs as the operator user, while its host supervisor
  runs as root with `CAP_DAC_OVERRIDE` removed. A `0600` operator-owned key is
  therefore correctly unreadable by the supervisor even though both services
  are intended to present the same Agent service identity.
- Copying the key or minting a second identity would split one service
  principal into extra custody and admission state. Restoring
  `CAP_DAC_OVERRIDE` would weaken the supervisor beyond the access it needs.
- A dedicated service group, a `0750` key directory, and a `0640` private key
  express the actual local custody set directly. The default remains the
  stricter single-user shape.

## Effect

The session-auth module now exposes `privateKeyMode`, restricted to owner-only
or owner-and-group read/write combinations. Its default remains `0600`. The
provisioning unit applies that declared mode after secure key creation, and the
module fixture proves both the default and the deliberate group-readable shape.

## Open questions

None for the current file-backed identity model. A future auth agent could
replace shared file readability with signing operations, but that remains a
separate custody design and is not required for this service boundary.
