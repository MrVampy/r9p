# Git Smart Protocol Over An Authenticated Stream

## Question

What is the smallest reusable r9p mechanism needed for a Git repository owner
to offer native incremental fetches over an authenticated 9P data channel?

## Sources inspected

- `refs/git/Documentation/git-remote-ext.adoc` in the Coordinator workspace for
  Git's external-command smart-transport contract and `%G` service request.
- `refs/git/Documentation/gitprotocol-pack.adoc` for upload-pack negotiation and
  transport-independent pack transfer.
- `refs/git/Documentation/git-upload-pack.adoc` for the read-side repository
  service and its security boundary.
- `refs/git/Documentation/git-daemon.adoc` and `refs/git/daemon.c` for the
  inetd process shape, strict path admission, and read-only upload-pack default.
- `crates/cli/src/commands/stream.rs` and
  `crates/cli/tests/cli_stream.rs` for r9p's existing byte-transparent,
  non-replaying full-duplex client.
- `crates/core/src/server/connection.rs` for asynchronous request,
  cancellation, and connection-reset behavior.
- `crates/auth/src/handshake.rs` and `crates/reverse/src/export.rs` for verified
  peer identity reaching an application-owned handler factory.
- GitButler `grit-git/src/ext_transport.rs` and HeddleCo
  `crates/sley-remote/src/ssh.rs` for independent Rust implementations that
  parse Git's `ext::` command, emit the `%G` git-daemon request, and carry the
  smart protocol over child stdin and stdout.
- GitLab Geo repository synchronization and disaster-recovery documentation
  for event-driven native repository sync, separate verification state,
  read-only secondaries, and explicit promotion.

## Findings

Git already owns the repository-specific mechanism. `remote-ext` treats one
external command as a full-duplex byte channel. With `%G`, Git writes the
standard git-daemon service and repository request before normal upload-pack
negotiation. The remote upload-pack advertises refs, receives the client's
existing object set, and sends only the missing objects in a pack. A transport
adapter must preserve bytes and lifecycle; it must not parse Git packets,
construct bundles, or update refs itself.

The public searches found no maintained Git-over-9P transport to adopt.
GitButler's grit and HeddleCo's sley nevertheless confirm the generic pattern:
their ext transports spawn an arbitrary command, connect stdin and stdout, and
leave Git protocol semantics to the upload-pack implementation.

r9p already has the client half in `r9p stream`. Its server facade supports a
per-connection asynchronous handler, verified peer identity, cancellation, and
reset. The missing reusable half is therefore an authenticated process-stream
exporter with these bounds:

- one fixed absolute executable and argument vector selected by the service
  owner, never a client-selected command or shell string;
- an exact configured certified-principal allowlist checked before process
  construction;
- one fresh process group per admitted 9P connection;
- separate read and write fids with contiguous non-replaying offsets;
- a bounded output buffer with OS-pipe backpressure on input;
- a bounded concurrent-session count; and
- process-group termination on disconnect, reset, or cancelled session
  teardown.

## Decision

Add `r9p stream-export` as the generic server-side counterpart to `r9p stream`.
It exports `/stream` and runs one fixed process per admitted authenticated
session. It remains ignorant of Git.

A Git-owning application with one fixed working-tree repository can run
`git upload-pack --strict <repository>/.git` behind that stream and use Git's
built-in `ext::r9p ... stream /stream` URL on the consumer. This keeps the
host-local repository path entirely at the owner. A multi-repository owner can
instead run `git daemon --inetd` and use `%G<repository>` for the standard
git-daemon request. In both forms, the application owns repository path
admission, head publication, fetch verification, and promotion policy.
Coordinator remains a coordination plane and does not relay the Git pack
bytes.
