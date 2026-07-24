# Delimiter-Terminated Record Reads

Date: 2026-07-24.

## Question

How should a retained 9P client consume one bounded record from a blocking
file without issuing an additional read merely to discover EOF?

## Sources Inspected

- `crates/core/src/multiplex/client.rs`
- `crates/core/src/client_support.rs`
- `crates/session/src/client.rs`
- `crates/session/src/opened_fid.rs`
- `refs/plan9port/include/9pclient.h`
- `refs/plan9port/src/lib9pclient/read.c`
- `refs/plan9port/src/cmd/9p.c`
- `refs/plan9port/src/libbio/brdline.c`
- `refs/plan9port/src/libbio/brdstr.c`

## Live Finding

A retained terminal observer read one newline-terminated JSON update with
`read_full`. That operation continued after receiving the complete record
because it could stop only after a later read returned EOF. Under closely
spaced terminal updates, the extra read occasionally produced another
fragment. The consumer then rejected the combined bytes as JSON with
`trailing characters at line 2`.

The ordinary plan9port client API keeps one fid offset and exposes both
single-read and exact-count operations. Record-oriented consumers such as
event readers stop when their application framing is complete; they do not
require a synthetic EOF round trip after every record. Plan9port's Bio layer
similarly treats a delimiter as the completion boundary for a logical line.

## Design

The generic multiplexed client and session facade now expose a bounded
delimiter-terminated read. It:

- includes the delimiter in the returned bytes;
- continues across partial 9P reads when the record exceeds one response;
- stops immediately when the delimiter arrives;
- rejects bytes after the delimiter in the same response because the
  stateless operation cannot retain them for another record;
- rejects EOF or exhaustion of the caller's byte bound before the delimiter;
  and
- has both deadline-bound and intentionally unbounded blocking variants.

This is a generic 9P session mechanism. It contains no terminal, service,
namespace-policy, or JSON semantics. A consumer chooses the delimiter and
bound required by its own record contract.

## Effect

Blocking record files no longer need an EOF probe after their application
framing has already completed. This removes one request-response round trip
from the common one-chunk case and prevents a later dynamic state transition
from being mistaken for trailing bytes in the completed record.

The r9wm terminal client is the first consumer. Its terminal namespace
contract uses one newline-terminated JSON document per read or RPC response.

## Open Questions

None for the bounded one-record operation. Multi-record streams need a
buffered reader that preserves bytes following a delimiter; they must not use
this stateless method.
