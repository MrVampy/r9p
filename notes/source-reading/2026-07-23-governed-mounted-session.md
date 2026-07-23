# Governed mounted service sessions

Date: 2026-07-23

## Question

How should a client preserve one logical 9P namespace when a governance
endpoint resolves and admits a service but the established service session
must bypass that endpoint?

## Sources inspected

- `../9front/sys/src/libc/9sys/dial.c`, especially `csdial`
- `../9front/sys/src/cmd/srv.c`, especially `main` and `post`
- `../9front/sys/src/cmd/mount.c`, especially `amount0`
- `../9front/sys/src/9/port/portdat.h`, especially `Mount` and `Mhead`
- `../9front/sys/src/9/port/chan.c`, especially `cmount`, `findmount`, and
  `domount`
- `refs/plan9port/src/cmd/9import.c`, especially `connectez` and
  `post9pservice`
- `crates/core/src/connection_descriptor.rs`
- `crates/session/src/client.rs`
- `crates/session/src/opened_fid.rs`

## Findings

Plan 9 separates resolution, connection establishment, and namespace
composition:

- `csdial` writes a symbolic destination to `/net/cs`, reads concrete
  candidates, closes the connection-server fid, and dials a selected network
  endpoint.
- `srv` posts the resulting live file descriptor under `/srv`; `mount`
  authenticates that channel and attaches it to a namespace path.
- The kernel namespace maps the mounted-upon `Chan` to a different mounted
  `Chan`. `findmount` and `domount` cross that boundary during ordinary path
  traversal.
- plan9port `9import` follows the same shape: it dials and authenticates the
  export, then passes the connected descriptor to `post9pservice` for
  publication and optional mounting.

The logical namespace is therefore one composed tree, but it is not one
physical transport. Operations below a mount travel on the mounted service
channel. The resolver or governance channel does not relay those operations.

## Effect on r9p

The session layer should provide a generic composed namespace client:

- retain the root/governance client for paths it owns;
- resolve a service through the root namespace;
- establish the authenticated service client from the returned
  `r9p-connection.v1` descriptor;
- mount that client at a caller-declared public prefix;
- rebase paths below the prefix onto the descriptor's exported root;
- return ordinary retained `OpenedFid` values backed by the selected client.

Resolution policy and mount selection remain outside the protocol core. The
session adapter only implements generic client-side namespace composition.

## Open questions

- A connection descriptor names the exported root but not one selected public
  mount path because a registered service can publish at several paths. The
  caller must currently supply the governed public mount path explicitly.
- Lease expiry bounds use of a descriptor to establish a session. Established
  session revocation remains an application authority concern and must not
  require relaying every 9P operation through the resolver.
