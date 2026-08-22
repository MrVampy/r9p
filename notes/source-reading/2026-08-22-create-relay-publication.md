# Create relay publication

## Consumer

Newsgroups Plan 03 Slice 2 creates durable request directories and must publish
each directory's request, status, progress, wait, coverage, and results entries
before the successful create operation returns to its namespace caller.

## Source finding

`FrontTree::create` previously inserted an owner-accepted node only after
`Front::complete_create` woke the client-side tree. The owner therefore could
not publish children under an accepted directory before `Rcreate` became
observable. Publishing before completion collided with the later insertion;
publishing afterward exposed a transient incomplete directory.

The relevant source paths are `crates/front/src/tree.rs`,
`crates/front/src/front.rs`, and `crates/front/src/model.rs`. The existing
mutation-relay tests established that the owner supplies the authoritative qid
and that the created fid must remain bound to that exact node.

## Resolution

Accepted create completion now installs the node in shared Front state before
the reply is published. The Rust-linked `complete_create_with` form runs one
bounded owner publication step after insertion and before wakeup. A failed
publication removes the inserted subtree and rejects the create, so callers
never observe a successful but structurally incomplete directory. The existing
`complete_create` and C ABI keep their current surface through a no-op
publication step.
