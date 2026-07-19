# Same-fid RPC request chunks

Date: 2026-07-20

## Question

Where should reusable validation live when a write-then-read RPC request is
larger than one negotiated 9P write payload?

## Sources inspected

- `crates/core/src/client_support.rs`: `write_in_chunks`.
- `crates/core/src/blocking.rs`: `Client::write` and `Client::rpc_path`.
- `crates/front/src/tree.rs`: `FrontTree::write` and `read_target_at`.
- `crates/front/tests/conformance.rs`:
  `rpc_path_buffers_request_larger_than_negotiated_write_payload`.
- `refs/plan9port/src/lib9p/srv.c`: `swrite`.
- `refs/plan9port/src/lib9p/ramfs.c`: `fswrite`.
- The Agents service's same-fid RPC buffer and the Credentials service's
  single-write RPC implementation, as downstream consumers.

## Findings

Each `Twrite` has its own offset and protocol-bounded payload. The blocking
client therefore splits a larger logical write into contiguous protocol-sized
writes. Plan9port passes each write and its offset to the file implementation;
ordinary file implementations apply those writes at the declared offsets.

Write-then-read RPC is a convention layered above 9P. The existing r9p front
already treats offset zero as the start or replacement of a request, buffers
later exactly contiguous writes on the same fid, and submits the complete
request when the first read arrives. Its conformance test proves a 9,500-byte
request over an 8,192-byte negotiated message size. Agents independently uses
the same shape. Credentials was the outlier: it executed each write immediately
and rejected a continuation offset.

The reusable part is validation of request start/replacement, contiguous
offsets, and per-request/per-connection bounds. Buffer ownership and execution
remain application concerns: a credential service needs zeroizing request
storage, while a generic front also carries routing context and asynchronous
request state.

## Effect

`crates/core/src/rpc.rs` now exposes bounded request-chunk validation without
owning bytes, policy, runtime state, or a transport. Downstream services can use
their appropriate buffer type and execute only when their same-fid RPC
convention declares the request complete.

## Open questions

The front and Agents implementations can adopt the shared validator in a later
mechanical cleanup. Their current behavior is already correct, so that cleanup
is not required for the Credentials fix.
