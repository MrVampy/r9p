# Export Auth Boundary Consumer Adoption

Date: 2026-07-22.

## Question

Should r9p restore the retired `wg:<network>` descriptor form when
trading-core adopts the current shared r9p revision?

## Sources inspected

- `crates/core/src/export_descriptor.rs`, especially `AuthBoundary::parse`,
  `AuthBoundary::validate`, and `ExportDescriptor::validate_authority_boundary`
- `bindings/gleam/src/r9p/export_descriptor.gleam`, especially `auth_class` and
  `validate_authority`
- `crates/front/bindings/deno/export_descriptor.ts`, especially
  `validateOptions`
- trading-core registration fixtures and its Nix deployment defaults
- M7's r9p session-auth domain configuration

## Findings

- The older descriptor grammar used `wg:*` and `tailscale:*` as network
  placement labels. The current contract instead names the authentication
  mechanism that protects the 9P session: `p9any:noise-ik@domain`,
  `uds-peercred:*`, or loopback-only `none`.
- Rust, Gleam, and Deno deliberately enforce the same current grammar and
  transport compatibility rules.
- M7's trading service is a loopback backend and receives `auth=none` from its
  deployment module. The failing non-loopback test fixture was stale; the
  configured authenticated namespace domain is `vault`.
- Restoring `wg:*` would conflate network placement with authenticated session
  authority and introduce a compatibility form with no current deployment
  consumer.

## Effect

r9p remains unchanged. Trading-core's non-loopback registration fixtures move
forward to `p9any:noise-ik@vault` while its deployed loopback descriptor remains
`auth=none`. The shared r9p contract stays the single definition of descriptor
authentication semantics.

## Open questions

None for this adoption. Any future non-loopback service export must advertise
the exact r9p session-auth mechanism and domain it actually uses.
