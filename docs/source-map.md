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
  - P9any provider negotiation, Noise XX mutual-certificate authentication,
    authenticated record
    framing, Unix peer attestation, key material, and typed client/server
    session configuration.
  - `SecureStream` is itself an authentication transport, allowing an
    end-service session to be layered over an authenticated reverse-placement
    stream without teaching the broker application identity.
  - It exposes a verified certified-name or Unix-peer transport subject. Only
    an explicitly local trust transport may preauthorize a 9P username; it
    carries no remote-key bootstrap allowlist, backend admission, or namespace
    policy.
- `docs/design/auth-agent.md`
  - Deferred local long-term-key custody boundary, its concrete activation
    triggers, and the unresolved Snow external-static-DH or raw-split seam.
- `docs/design/certificate-lifecycle.md`
  - Boundary between the completed mutual-certificate transport and future
    issuance, renewal, rotation, revocation, and offline-root lifecycle.
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
    boundary through the placement stream. The peer-aware handler factory also
    receives the verified principal, public key, and certificate groups for
    application-owned authorization. Connection availability is an event-driven
    wait, and reverse or proxy listeners block until work or an explicit
    shutdown wake arrives.
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
- `crates/session/src/feed/`
  - Generic namespace-change subscription over a mandatory blocking 9P stream.
  - The recent or cursor path is a one-shot catch-up source after connection
    loss, never a periodic polling source. The configured delay bounds
    reconnect attempts only.
- `crates/session/src/projection/`
  - Private local projection of one authenticated namespace subtree.
  - Each local session gets its own upstream client; shutdown uses an internal
    event-driven wake descriptor and never depends on reconnecting to the
    published socket.
- `docs/guides/event-driven-9p.md`
  - Generic retained-fid, blocking-read, tagged-concurrency, cancellation,
    backpressure, and reconnect pattern for event-driven 9P applications.
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
  - `r9p stream` uses distinct read-only and write-only fids over the same
    full-duplex transport as a byte-transparent, non-replaying machine stdio
    adapter.
  - Ordinary path commands use the transparent namespace client;
    `--auth-domain` names the responder a root dial requires.
- `crates/cli/tests/cli_machine.rs`
  - Machine-output and streaming command regression tests.
- `crates/cli/tests/cli_stream.rs`
  - End-to-end full-duplex byte transparency, including control, carriage
    return, non-UTF-8, and multi-frame input.
- `crates/fuse/src/`
  - Canonical Linux FUSE bridge over the `r9p` client primitives, exposed as
    `r9p mount`.
  - Its direct change-feed adapter uses the same stream-primary and
    cursor-catch-up contract; session-hosted mounts consume the session feed's
    event bus.
- `notes/source-reading/2026-07-29-qid-version-is-not-inode-identity.md`
  - Source and live evidence that `qid.version` is modification state, not
    inode identity, and that remappers and FUSE generations must retain a
    stable identity across version changes.
- `notes/source-reading/2026-08-06-native-dns-endpoints.md`
  - Source-backed boundary between concrete listener binds and DNS-preserving
    dial or referral endpoints.
- `notes/source-reading/2026-08-08-auth-agent-and-certificate-lifecycle.md`
  - Source-backed reason to defer a full auth agent, the actual Snow transport
    handoff constraint, and the lifecycle responsibilities that precede it.
- `notes/source-reading/2026-08-10-atomic-front-recovery.md`
  - Source-backed boundary for publish-before-prune Front reconciliation and
    Coordinator-owned lease recovery after a publisher restart.
- `notes/source-reading/2026-08-10-desired-state-file-reconciliation.md`
  - Source-backed boundary for higher-level replace-or-create publication
    without replaying an ambiguous 9P write.
- `notes/source-reading/2026-08-10-namespace-projection-shutdown.md`
  - Source-backed boundary for projection lifecycle control independent of
    published Unix socket ownership and permissions.
- `notes/source-reading/2026-08-12-managed-fuse-active-request-shutdown.md`
  - Live proof that managed shutdown must abort the FUSE connection before it
    joins change-feed and request workers with active kernel requests.
- `notes/source-reading/2026-08-11-managed-fuse-shutdown.md`
  - Source-backed managed FUSE shutdown wake that does not depend on reconnecting
    to a published endpoint.
- `notes/source-reading/2026-08-11-fusermount-guardian-custody.md`
  - Source-backed ownership of the `auto_unmount` liveness socket and resident
    `fusermount3` helper through normal managed teardown.
- `notes/source-reading/2026-08-11-lazy-directory-reads.md`
  - Source-backed boundary between incremental FUSE directory handles,
    per-fid Front directory snapshots, and application-owned page loading.
- `crates/fs/src/`
  - Local filesystem-backed 9P server adapter used by `r9p serve` and
    `r9p export`; read-only by default with an explicit writable mode.
- `crates/front/src/abi/client.rs`
  - Generic front ABI operations over the transparent namespace client.
- `crates/beam-port/src/lib.rs` and `bindings/gleam/`
  - BEAM target encoding and caller-local authority bindings for the same
    namespace client.
  - Tagged native-port multiplexing lets a blocking Front request intake
    remain pending while independent BEAM processes publish and complete work
    against the same Front.

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
