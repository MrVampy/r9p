# Negotiated Bulk 9P Message Size

Date: 2026-08-11

## Question

Is r9p's 64 KiB maximum message size a protocol or transport requirement, or
can authenticated namespace sessions negotiate a larger message size for bulk
regular-file reads?

## Sources Inspected

- `crates/core/src/codec.rs`
- `crates/core/src/server/config.rs`
- `crates/core/src/multiplex/client.rs`
- `crates/auth/src/stream.rs`
- `crates/front/src/serve.rs`
- `crates/cli/src/io.rs`
- `refs/plan9port/src/lib9p/srv.c`
- `refs/plan9port/src/lib9pclient/fs.c`
- `refs/plan9port/src/lib9pclient/read.c`
- `refs/coordinator/refs/linux-kernel/include/net/9p/client.h`
- `refs/coordinator/refs/linux-kernel/net/9p/client.c`
- `refs/coordinator/refs/diod/src/libdiod/diod_ops.c`
- `refs/coordinator/refs/diod/src/libnpclient/read.c`

## Findings

- 9P encodes `Tversion.msize` and `Rversion.msize` as 32-bit values. Read and
  write payloads are bounded by the negotiated message size minus their wire
  headers, not by a 64 KiB protocol field.
- plan9port allocates buffers from the negotiated message size and clamps each
  read to that size minus `IOHDRSZ`. Its server and client code do not impose a
  64 KiB protocol ceiling.
- Linux v9fs requests 128 KiB of payload plus protocol overhead by default and
  accepts a smaller server response.
- Diod sets its server maximum message size to 1 MiB and likewise clamps each
  client read to the negotiated bound.
- r9p's authenticated stream uses length-bounded encrypted records, but its
  `Write` implementation returns a partial write and standard `write_all`
  semantics split a larger 9P frame across as many encrypted records as needed.
  The encrypted record boundary is not a 9P frame boundary.
- A live 4 MiB newsgroups artifact read completed in about 0.09 seconds inside
  NucBox and about 1.3 to 1.6 seconds across the mesh ingress. At 64 KiB per
  request, network round trips dominated the transfer.

## Effect

r9p now permits a negotiated maximum message size of 1 MiB. The CLI requests
that size and uses it as its ordinary read chunk. Servers remain free to
negotiate any smaller supported size, so established Plan 9 and 9P servers keep
their own limits without a compatibility path.

Regression coverage proves that the codec accepts a maximum-sized bulk read
response and that the authenticated transport carries one 1 MiB write across
multiple encrypted records without changing the plaintext byte stream.

## Open Question

The 1 MiB bound increases the maximum allocation for one admitted frame. Live
service metrics should continue to distinguish message-size pressure from the
existing independent limits on concurrent requests and fids.
