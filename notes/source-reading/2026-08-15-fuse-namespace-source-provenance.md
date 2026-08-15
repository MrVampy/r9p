# FUSE namespace source provenance

Question: how can a generic application recover the logical 9P source of a
local `r9p mount` path without knowing the mountpoint or the consuming app?

Sources inspected:

- `crates/fuse/src/fuse/mount.rs`: the direct `fusermount3` invocation and its
  current option construction.
- `crates/fuse/src/fuse/config.rs`: the canonical absolute namespace source
  carried by every mount configuration.
- `refs/libfuse/lib/mount.c`, `refs/libfuse/lib/mount_util.c`, and
  `refs/libfuse/doc/mount.fuse3.8`: `fsname` becomes the mount table source and
  `subtype` identifies the FUSE implementation.
- `refs/linux-kernel/fs/fuse/inode.c`: the kernel accepts the FUSE subtype as
  mount context state.

Finding:

The bridge already has the exact logical source in `Config.source_path`, but
the FUSE mount was created without `fsname` or `subtype`. Linux therefore
reported only `/dev/fuse`, discarding the provenance needed to translate a
local path back into the composed namespace.

Decision:

Every r9p FUSE mount now declares subtype `r9p` and an `fsname` containing the
percent-encoded absolute namespace source with an `r9p:` marker. Encoding the
UTF-8 bytes keeps commas, spaces, backslashes, and other mount-option syntax
out of the option value. Generic consumers can select the deepest enclosing
`fuse.r9p` mount from `/proc/self/mountinfo`, decode its source, and append the
local relative path. The local status document also reports the unencoded
`namespace_source` for diagnostics.

This is FUSE bridge provenance only. It does not add namespace policy or any
consumer-specific path knowledge to r9p.
