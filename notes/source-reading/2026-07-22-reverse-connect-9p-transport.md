# Reverse-Connect 9P Transport

Date: 2026-07-22.

## Question

How can a filesystem-owning host behind an inbound firewall publish an ordinary
9P tree without introducing SSH, a file synchronization lane, or
application-specific protocol machinery?

## Sources inspected

- `crates/auth/src/handshake.rs`, especially `authenticate_client` and
  `authenticate_server`
- `crates/auth/src/stream.rs`, especially `SecureStream::try_clone` and its
  `Read`, `Write`, and `ConnectionStream` implementations
- `crates/core/src/server/connection.rs` and
  `crates/core/src/server/file_tree_handler.rs`
- `crates/cli/src/commands/serve/runtime.rs`
- `crates/fs/src/lib.rs`, especially `LocalTree::open_with_config`
- `crates/core/src/blocking.rs`, especially `Client::connect_with_variant`

## Findings

- The Noise IK session layer already authenticates and encrypts an arbitrary
  TCP stream before 9P starts. Its `SecureStream` supports independent cloned
  read and write handles and is already an ordinary generic 9P connection
  stream.
- The generic server accepts any `ConnectionStream`; it does not require the
  9P server to have called `accept(2)` on the underlying TCP socket. Therefore
  the filesystem owner may connect outward and then serve 9P on that stream
  without changing a single 9P message.
- A broker can pair that authenticated outward stream with a loopback client
  connection by copying bytes bidirectionally. It does not parse messages,
  own fids, translate filesystem behavior, or acquire the exported authority.
- `LocalTree` remains the correct filesystem adapter. The reverse transport
  changes connection placement only; read-only versus writable behavior and
  path containment remain owned by the existing adapter.
- A bounded pool of one-session reverse streams fits existing one-connection
  9P server semantics and avoids inventing another multiplexing protocol.

## Effect

The `r9p-reverse` runtime-adapter crate provides a generic authenticated broker
and reverse filesystem exporter. The protocol core, filesystem adapter, Vault
policy, and application admission remain unchanged.

## Open questions

None for the initial bounded transport. A future need for long-lived
multi-session stream multiplexing should be justified by measurements before
adding a framing layer.
