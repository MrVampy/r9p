# Referral Ancestor Walks

Date: 2026-07-31

## Question

How should an ordinary component-by-component client walk reach a referral
mounted below namespace parents that the admitted root does not itself serve?

## Sources Inspected

- `crates/core/src/referral.rs`
  - `NamespaceReferral::validate`
  - `NamespaceReferral::routed_path`
- `crates/session/src/client/namespace.rs`
  - `Client::walk_namespace_path`
  - `Client::walk_namespace_path_timeout`
  - `Client::routed_target`
- `crates/session/src/client/namespace/routing.rs`
  - `mounted_suffix`
  - `walk_remote`
  - `walk_remote_timeout`
- `crates/session/src/client/tests.rs`
  - `ordinary_namespace_operations_cross_referrals_transparently`
  - `walk_miss_refreshes_referrals_added_after_attach`
- `crates/fuse/src/fuse/ops/lookup.rs`
  - `R9pFuse::lookup`
- `crates/fuse/src/fuse/ops/io/open.rs`
  - `R9pFuse::open`
- `crates/fuse/src/fuse/ops/dir.rs`
  - `R9pFuse::readdir`
  - `R9pFuse::readdirplus`
- `crates/session/src/cache.rs`
  - `read_open_directory_entries`

## Findings

- A full path such as `/agents/runtime/m7/guide` selects the longest matching
  referral before issuing a remote walk.
- FUSE resolves the same path one component at a time. The first lookup walks
  `/agents`, which has no full referral match and may be absent from the root
  tree even though it is a strict ancestor of an admitted mount.
- Referral records must remain protocol mechanism rather than public files.
  The logical namespace client can still expose the necessary parent shape as
  read-only directories derived from admitted mount paths.
- A real root directory takes precedence. A referral-derived directory is used
  only after the root walk returns `ENOENT`.
- The derived directory must support stat, read-only open, directory reads,
  component walks, local clone, and local clunk. Mutations are rejected.
- Referral freshness still controls whether an unconnected route can supply
  namespace shape. An already connected route remains usable after the
  referral's connection-establishment deadline.

## Effect

The session namespace client now derives missing strict referral ancestors as
read-only directories. This makes ordinary fid walks and the FUSE bridge use
the same referral machinery as full-path CLI operations without adding
Coordinator or service-specific behavior.

## Open Questions

- Root and real-directory listings are not currently unioned with referral
  children. Explicit lookup and all descendant operations are compositional,
  but listing an existing real parent can still reflect only the root
  server's entries. This should be addressed only if the composed namespace
  contract requires union-directory enumeration.
