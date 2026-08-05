# Directory Read And CLI Semantics

Date: 2026-07-23

## Question

Should `r9p read` reject directories because packed directory entries can
contain terminal control bytes, or is streaming those bytes the established
Plan 9 behavior?

## Sources Checked

Current `r9p` behavior:

- `crates/cli/src/commands/read_write.rs`: `read_cmd`.
- `crates/cli/src/commands/ls.rs`: `ls_one` and `read_dir_stats`.
- `crates/cli/src/io.rs`: `copy_fid_to_stdout`.
- `crates/core/src/blocking.rs`: `read_path`, `list_path`, and
  `read_dir_stats`.
- `crates/core/src/stat.rs`: `dirread_chunk` and `decode_dir_entries`.
- `README.md` and `docs/design/architecture.md`: the negotiated protocol variants.

Plan 9 lineage and user commands:

- `refs/vault/refs/9front/sys/man/5/read`,
  `sys/man/5/stat`, and `sys/man/2/dirread`.
- `refs/vault/refs/9front/sys/src/cmd/cat.c` and `sys/src/cmd/ls.c`.
- The corresponding 9legacy manuals and command sources under
  `refs/vault/refs/9legacy`.
- `refs/vault/refs/inferno-os/man/5/read`, `man/2/styx`,
  `man/2/sys-dirread`, and `appl/cmd/cat.b`.
- `refs/plan9port/src/cmd/9p.c`,
  `src/lib9pclient/read.c`, `src/lib9pclient/dirread.c`, and
  `man/man1/9p.1`.
- The same paths in `refs/vault/refs/plan9port-vault`; the relevant files are
  byte-for-byte identical to upstream plan9port.

Other plain 9P2000 or 9P2000.u implementations:

- `refs/knusbaum-go9p/client/client.go` and `fs/server.go`.
- `refs/lionkov-go9p/p/clnt/read.go`,
  `p/clnt/examples/ls/ls.go`, and the server read paths.
- `refs/vault/refs/py9p/py9p/py9p.py`,
  `examples/cl.py`, and `9pfs/9pfs`.
- `refs/vault/refs/arigato/src/server/message_handler.rs` and
  `examples/p9srv/src/file_server.rs`.

Multi-dialect and 9P2000.L implementations:

- `refs/lib9p/request.c`, `backend/fs.c`, and
  `pytest/protocol.py` plus `pytest/p9conn.py`.
- `refs/vault/refs/p92000l-rust/src/client.rs`, `server.rs`, and
  `fcall.rs`.
- `refs/vault/refs/rust-p9/src/server/mod.rs`,
  `server/read_dir.rs`, and `protocol/messages.rs`.
- `refs/vault/refs/rust-9p/src/srv.rs` and `fcall.rs`.
- `refs/vault/refs/rs9p/crates/rs9p/src/srv.rs` and `fcall.rs`.
- `refs/vault/refs/r9-fileserver/src/core/srv.rs` and `fcall.rs`.
- `refs/vault/refs/zerofs/zerofs/src/ninep/handler.rs` and
  `protocol.rs`.

Bridge and catalog references:

- `refs/vault/refs/9pfuse/main.c`.
- `crates/fuse/src/`.
- `refs/vault/refs/awesome-9p` is a discovery catalog and does not define
  protocol or CLI behavior.
- Linux FUSE and libfuse references define the FUSE presentation boundary,
  not 9P directory-read semantics.

The optional top-level `refs/r9pfuse` and `refs/racme` symlinks are currently
broken. The former is a retired bridge and the latter records the extraction
boundary; neither owns the wire or CLI contract. The broken top-level
`refs/9pfuse` and `refs/plan9port-vault` links have readable equivalents under
`refs/vault/refs/`, which were checked.

## Findings

### Plain 9P2000 reads directories with `Tread`

The 9front, 9legacy, and Inferno protocol manuals agree: reading a directory
returns an integral number of machine-independent directory entries encoded
like stat records. Directory offsets must continue at record boundaries.

The plain 9P2000 and 9P2000.u libraries follow that contract. Their ordinary
read operations return raw reply data, while their `dirread`, `Readdir`,
`lsdir`, or equivalent helpers decode the same bytes into typed stat entries.
The arigato file-server example makes the representation especially explicit:
opening a directory constructs a cursor containing dehydrated stat records,
and ordinary reads consume that cursor.

### The established user commands do not protect the terminal

Plan9port `9p read` opens the path with `OREAD`, loops over `fsread`, and writes
every returned byte to standard output. It does not stat the path or reject a
directory. Its `9p ls` path instead calls `fsdirreadall` and formats the decoded
entries.

Native 9front and 9legacy `cat` likewise copy ordinary read bytes to standard
output without checking for a directory, while `ls` uses `dirreadall`.
Inferno `cat` has the same raw copy loop and its directory helpers decode
directory entries separately. The Vault-local plan9port fork retains upstream
behavior exactly in the relevant files.

Therefore, raw directory bytes reaching a terminal are hazardous but not a
departure from the established command contract. The same general hazard
exists when `cat` is applied to any binary file containing terminal control
bytes.

### 9P2000.L deliberately uses a different operation

9P2000.L adds `Treaddir` and `Rreaddir` with Linux-style directory entries.
The multi-dialect lib9p reference explicitly states that `Tread` cannot be used
on directories under 9P2000.L. The 9P2000.L Rust references all keep file
`read` and directory `readdir` as separate protocol operations; rust-p9 states
the restriction directly in its read handler.

This is a dialect distinction, not evidence that a plain 9P2000 client should
reject directory reads. `r9p` intentionally negotiates plain `9P2000` and does
not claim 9P2000.L.

### Current r9p matches the plain 9P2000 lineage

`r9p read` and `r9p cat` stream the bytes returned by ordinary 9P reads.
`r9p ls` reads those bytes and passes them through `decode_dir_entries`.
The blocking client similarly exposes both `read_path` for raw bytes and
`list_path` for decoded stat entries.

Changing `r9p read` to reject directories would break the plan9port-shaped CLI
contract and remove a valid plain 9P2000 primitive. It would also make the CLI
inconsistent with the core protocol variant it negotiates.

## Effect

Do not add a directory rejection to `r9p read`, `cat`, or `readfd`.
Use `r9p ls` when the intended operation is a human-readable directory
listing.

The tmux corruption was caused by a valid raw byte stream being interpreted as
terminal control state. A durable defense belongs at the tool-output rendering
boundary, where untrusted control bytes should be escaped or otherwise kept
from the terminal emulator. It should not be implemented by changing plain
9P2000 read semantics.

## Open Questions

- Identify the narrow Codex output-rendering boundary that allowed captured
  command bytes to become terminal control input.
- Decide separately whether the broken optional reference symlinks should be
  repaired as repository-workspace hygiene.
