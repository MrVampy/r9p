# Source Map

This map defines the local sources agents should inspect before making source-specific `r9p` claims.

## r9p Source

- `crates/core/src/codec.rs`
  - 9P frame encoding/decoding.
  - Message-size math, read/write payload limits, stat-entry chunking.
- `crates/core/src/message.rs`
  - T-message and R-message shape.
  - Tags, `NOTAG`, and `Treferrals` or `Rreferrals` message identities.
- `crates/core/src/fid.rs`
  - Fid lifecycle state, open mode, directory offset, and `NOFID`.
- `crates/core/src/mode.rs`
  - Canonical 9P open-mode constants and access predicates.
- `crates/core/src/flush.rs`
  - Live-tag table, duplicate-tag rejection, flush and stale-completion behavior.
- `crates/core/src/server/`
  - Generic file-tree trait, connection adapters, session state, fid
    reservations, and open/read/write/stat/walk handling.
- `crates/core/src/client.rs`
  - Runtime-neutral client operation builder and response admission.
- `crates/core/src/blocking.rs`
  - Blocking client operations and the opt-in bounded TCP connection seam.
- `crates/core/src/blocking/tests.rs`
  - Blocking-client address, timeout, and stalled-handshake regressions.
- `crates/core/src/export_descriptor.rs`
  - Generic `r9p-export.v1` descriptor parsing, validation, and encoding.
  - It carries no service-registration lifecycle or runtime namespace policy.
- `crates/core/src/referral.rs`
  - Generic finite `9P2000.R` namespace referral shape and path rebasing.
  - Referrals carry admitted dial facts and a portable authority boundary
    without becoming files in the composed namespace.
- `crates/auth/src/`
  - P9any provider negotiation, Noise IK authentication, authenticated record
    framing, Unix peer attestation, key material, and typed client/server
    session configuration.
  - `SecureStream` is itself an authentication transport, allowing an
    end-service session to be layered over an authenticated reverse-placement
    stream without teaching the broker application identity.
  - It exposes a verified transport subject and may preauthorize a 9P username
    from a server bootstrap allowlist, but carries no backend admission or
    namespace policy.
- `crates/reverse/src/`
  - Generic authenticated reverse-connect runtime adapter.
  - An application-owned tree or filesystem owner connects outward and serves
    ordinary 9P over a bounded pool; the broker pairs those streams with a
    local endpoint by default. An explicit authenticated-network exposure may
    use a concrete TCP endpoint only when every bridged session has its own
    final service authentication boundary. The broker does not interpret 9P
    or own namespace policy.
  - `ReverseExport` owns the generic outbound lifecycle and accepts a fresh
    `FileTree` or asynchronous `ConnectionHandler` factory per session.
    Its authenticated variants establish an independent final service-client
    boundary through the placement stream. Connection availability is an
    event-driven wait, and reverse or proxy listeners block until work or an
    explicit shutdown wake arrives.
    `FilesystemExport` is the local-tree specialization used by the CLI.
- `crates/core/src/multiplex/`
  - Layered blocking transport facade for concurrent tagged client calls.
- `crates/session/src/client/`
  - One logical namespace client over a root session and lazily established
    direct referral sessions.
  - Longest-prefix routing, logical-to-remote fid binding, referral refresh,
    shared request tracking, and caller-local path operations.
- `crates/session/src/client_session.rs`
  - Renewable root attachment, current-client replacement, session epoch,
    serialized reconnect, and permanent shutdown.
  - It never decides whether a consumer operation is safe to replay or how
    application cursors and opened paths are rebuilt.
- `crates/session/src/authority.rs`
  - Caller-local bindings from portable authority boundaries to absolute
    session authentication configuration paths.
- `crates/session/src/opened_fid.rs`
  - Retained-fid operations over the same transparent namespace client.
- `crates/session/src/resumable_fid.rs`
  - Opt-in reattach and rewalk for a file whose read and write offsets are
    application-level replay cursors.
  - It retries only after definitive transport failure and does not make
    ordinary file writes idempotent.
- `crates/core/src/stat.rs`
  - 9P stat record shape and mode helpers.
- `crates/core/tests/memory_tree.rs`
  - Minimal end-to-end server/client fixture.
- `crates/core/tests/server_protocol.rs`
  - Source-backed version, fid lifecycle, walk, directory-offset, and response
    bound regressions.
- `crates/cli/src/`
  - The `r9p` binary and one-shot client command dispatch.
  - `r9p con` retains two fids on one multiplexed 9P connection so stdin and
    stdout remain one logical application session.
  - `r9p con --resume` uses the same two lanes across renewed attachments only
    for an explicitly replay-safe offset stream.
  - Ordinary path commands use the transparent namespace client;
    `--authority-auth` supplies caller-local authority bindings.
- `crates/cli/tests/cli_machine.rs`
  - Machine-output and streaming command regression tests.
- `crates/fuse/src/`
  - Canonical Linux FUSE bridge over the `r9p` client primitives, exposed as
    `r9p mount`.
- `crates/fs/src/`
  - Local filesystem-backed 9P server adapter used by `r9p serve` and
    `r9p export`; read-only by default with an explicit writable mode.
- `crates/front/src/abi/client.rs`
  - Generic front ABI operations over the transparent namespace client.
- `crates/beam-port/src/lib.rs` and `bindings/gleam/`
  - BEAM target encoding and caller-local authority bindings for the same
    namespace client.

Use these when the question is "what does `r9p` do now?"

## Plan 9 And plan9port

- `refs/plan9port/src/cmd/9p.c`
  - plan9port one-shot 9P client command behavior.
- `refs/plan9port/man/man1/9p.1`
  - documented plan9port `9p` command behavior.
- `refs/plan9port/include/9pclient.h`
  - plan9port client library API.
- `refs/plan9port/src/lib9p/`
  - plan9port server library behavior.
- `refs/plan9port/man/man9/`
  - 9P message reference pages.
- `refs/plan9port/src/cmd/acme/xfid.c`
  - Acme 9P file behavior when an Acme-specific compatibility question appears.

Use plan9port when the question is "what does the established 9P ecosystem expect?"

For reverse-connect lineage, also inspect `refs/plan9port/src/cmd/9import.c`
and the Plan 9 `import`, `exportfs`, `cpu`, and `srv` manuals when present. They
show that the 9P server need not be the side that accepted the underlying
connection and that a service name can denote a posted channel rather than a
fixed listener.

## Racme

- `refs/racme/docs/decisions.md`
  - Decision 6: `r9p` as substrate primitive and extraction trigger.
- `refs/racme/docs/arch/9p-as-substrate.md`
  - Boundary between `r9p`, backends, consumers, transports, and OS bridges.
- `refs/racme/docs/plan/03-m2-headless-9p.md`
  - M2 protocol commitments and Acme adapter boundary.
- `refs/racme/crates/racme-acme/`
  - Acme backend consuming `r9p`.

Use Racme when changing extraction-boundary claims or Acme-backed server behavior.

## Historical r9pfuse

- `refs/r9pfuse/crates/r9pfuse/src/p9.rs`
  - Historical blocking TCP client facade that predated the workspace cutover.
- `refs/r9pfuse/crates/r9pfuse/src/fuse.rs`
  - Historical FUSE/POSIX-to-9P translation.
- `refs/r9pfuse/crates/r9pfuse/src/node.rs`
  - Historical nodeid, fid, and directory-entry bookkeeping.
- `refs/r9pfuse/docs/source-map.md`
  - Source map for FUSE bridge behavior.

Use `crates/fuse/src/` for all current mount-client work. Use `refs/r9pfuse`
only as optional bounded historical comparison when the retired source checkout
is present locally and a plan explicitly needs lineage.

## coordinator

- `refs/coordinator/docs/operations/9p-endpoint.md`
  - coordinator 9P listener policy and backend contract.
- `refs/coordinator/docs/operations/plan9port-client.md`
  - Current operator workflows for `r9p`, `r9p mount`, plan9port `9p`, and
    kernel `v9fs`.
- `refs/coordinator/docs/source-map.md`
  - coordinator source-grounding map.

Use coordinator when validating governed namespace expectations, admission, or
service-addressing behavior.

## FUSE References

- `refs/coordinator/refs/linux-fuse/include/uapi/linux/fuse.h`
  - Linux FUSE protocol ABI.
- `refs/coordinator/refs/linux-fuse/fs/fuse/`
  - Linux kernel FUSE implementation.
- `refs/coordinator/refs/libfuse/include/`
  - libfuse userspace API headers.
- `refs/coordinator/refs/libfuse/example/`
  - Mature FUSE filesystem examples.
- `refs/coordinator/refs/9pfuse/`
  - Current C `9pfuse` bridge behavior.

Use FUSE sources only for bridge behavior. They do not define 9P semantics.

## Source Reading Notes

Write source-reading notes under `notes/source-reading/`.

Each note should include:

- Date.
- Question being answered.
- Files/functions inspected.
- Source-backed findings.
- Effect on `r9p` docs, plans, or code.
- Open questions.
