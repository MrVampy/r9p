# Bounded endpoint dialer source reading

Date: 2026-07-19

## Question

Where should a reusable bounded connector for TCP and Unix 9P endpoints live,
so Vault applications do not each implement endpoint parsing and transport
setup?

## Sources inspected

- `crates/core/src/blocking.rs`
- `crates/core/src/blocking/tests.rs`
- `crates/cli/src/transport.rs`
- `crates/session/src/transport.rs`
- `crates/core/src/server/connection.rs`
- `crates/core/src/export_descriptor.rs`
- Vault `src/native/r9p_listener/src/service_relay.rs`
- Agents `crates/runner/src/service/registration.rs`
- Agents `crates/execution/src/credential_authority.rs`

## Findings

- The blocking core already owns typed TCP and Unix client construction, TCP
  address normalization, and the finite handshake timeout seam.
- The CLI and session crates each recognize Unix endpoint spellings above that
  core, while Agents currently assumes TCP in both registration and credential
  authority calls.
- Choosing a TCP or Unix stream and applying finite I/O timeouts is useful to
  any blocking 9P consumer. It remains independent of Vault namespace policy,
  service registration, and peer authorization.
- Unix peer credentials are host admission evidence, not 9P protocol state.
  They stay in the consuming server rather than entering `r9p::core`.

## Effect

- The TCP-only timeout value becomes `ConnectionTimeouts`.
- `r9p::blocking::connect_endpoint_with_timeouts` accepts TCP, `unix!`, and
  `unix:` endpoints and returns the existing boxed blocking client.
- The existing typed TCP constructor remains available and consumes the same
  timeout value.
- Agents can use one connector while still assigning separate operator and
  service-owned namespace endpoints.

## Open questions

- Unix domain socket connect does not use the TCP address-resolution timeout;
  read and write deadlines still apply before the 9P handshake begins.
