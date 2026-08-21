# Removable demand-resolved children

`FrontTree::resolve_child_directory` inserts an accepted synthetic child while
holding the Front state lock. That insertion is the only race-free point where
the child can receive both its lazy directory-read relay and any native remove
relay before the waiting walk returns.

Registering a remove relay after `complete_child_directory_resolution` is not
equivalent. Completion wakes the waiting walk, so another client can observe
or remove the child before a later registration call reaches it. Publishing the
directory before completion has the inverse race: a listing can observe an
empty ordinary directory before its lazy read relay exists.

The resolver therefore carries an explicit `ChildDirectoryRemoval` policy into
the pending resolution. `Forbidden` preserves inert catalog children.
`RelayToOwner` atomically assigns the exact resolved child path as its remove
relay when the accepted directory is inserted. The ordinary `Tremove` path in
`FrontTree::remove` remains unchanged: it emits one owner request, waits for
acceptance, and removes the projected subtree only after the owner accepts.

Sources inspected:

- `crates/front/src/front/child_directory_resolution.rs`
- `crates/front/src/tree/child_directory_resolution.rs`
- `crates/front/src/tree.rs`, `FrontTree::remove`
- `crates/front/src/model.rs`, `State::insert_pushed_child_directory`
