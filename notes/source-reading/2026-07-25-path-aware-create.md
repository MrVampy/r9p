# Path-aware create stays above Tcreate

Date: 2026-07-25

## Question

Should a high-level r9p create helper accept a relative path such as
`sources/x`, while the 9P `Tcreate` message continues to carry one path
element?

## Sources inspected

- `refs/plan9port/src/lib9pclient/create.c`
- `refs/plan9port/src/cmd/9p.c`
- `refs/plan9port/include/9pclient.h`
- `crates/session/src/client/paths.rs`
- `crates/session/src/client/namespace.rs`
- `crates/front/src/abi/client.rs`

## Findings

Plan9port separates the two levels explicitly:

- `fscreate` accepts a path, splits it at the final slash, walks the parent,
  and passes only the leaf to `fsfcreate`.
- `fsfcreate` is the wire-level operation. It places that single leaf in
  `Tcreate`.
- The `9p create` command uses the path-aware `fscreate` helper.

r9p already keeps raw create strict in the namespace client. The missing part
was the corresponding path-aware behavior in its higher-level `create_at` and
`create_write_at` conveniences.

## Effect on r9p

The session helpers now accept one canonical relative path below the supplied
parent, walk its intermediate parent components, and send only the final
element through the existing raw create operation. This keeps 9P wire semantics
unchanged while making hierarchical namespace creation work through every
adapter that already calls the shared session helper.

## Open questions

None for this change. Recursive directory creation remains a separate
operation; the parent directories must already exist.
