---
title: Demand-resolved child directories in Front
date: 2026-08-16
sources:
  - refs/plan9port/man/man9/walk.9p
  - crates/front/src/tree.rs
  - crates/front/src/front.rs
  - crates/front/src/model.rs
  - crates/front/src/tests/directory_relays.rs
  - notes/source-reading/2026-08-11-lazy-directory-reads.md
---

# Demand-resolved child directories in Front

## Question

How can an unknown child become walkable on demand without turning `Twalk`
into an eager read of that child's directory?

## Findings

Plan 9 walk resolves path elements one at a time. A failure on the first name
is an error, while a later failure returns the successfully walked prefix.
Front previously applied that rule only to children already present in its
resident tree. Its directory read relay begins later, when an opened directory
receives a read, and already keeps the listing snapshot on that fid.

The missing operation is therefore child establishment, not directory
enumeration. A directory can own a resolver request prefix for unknown direct
children and a distinct read prefix for the directories that resolution
creates. The resolver publishes only the child's directory metadata. It does
not return children or trigger the read prefix.

## Effect on the implementation

An unknown walk below a registered resolver emits one bounded request carrying
the exact child name and request context. Concurrent walks of the same parent
and name share that request. Successful completion installs one pushed direct
child with its publisher-owned qid, generation, modification time, and logical
length, and binds the configured directory read relay to it. Rejection and
timeout leave no child behind.

The first directory read remains the point where Front emits the separate read
request. This preserves the existing per-fid directory snapshot and page
completion contract. A walk with additional names retains ordinary partial-walk
semantics if a later child cannot be resolved.

Front caps distinct in-flight child resolutions. Resolution and read prefixes
must differ, so a host can route the two request kinds without inspecting
source-specific path conventions or payload content.
