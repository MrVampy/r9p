#ifndef R9P_FRONT_H
#define R9P_FRONT_H

/*
 * r9p front C ABI, version 5.
 *
 * Contract rules:
 * - r9p_front_abi_version() must return 5 before any other call is made;
 *   hosts reject a mismatch.
 * - r9p_front_new() returns an owned handle; every handle must be released
 *   exactly once with r9p_front_free(). Calls other than r9p_front_free()
 *   are thread-safe: they may be called from any thread concurrently.
 *   r9p_front_free() is the lifetime boundary and may only be called after
 *   every other in-flight call using that handle has returned and no future
 *   calls will be made.
 * - All (pointer, length) string arguments are UTF-8, caller-owned, and
 *   only borrowed for the duration of the call. The library never retains
 *   or frees caller memory.
 * - All byte payloads are copied in; results are copied out into
 *   caller-provided buffers. No buffer crosses the boundary by reference.
 * - Return codes: 0 ok, 1 timeout (next_request only), -1 invalid
 *   argument, -2 internal failure. r9p_front_request_copy returns the
 *   copied byte count, or a negative code.
 * - r9p_front_last_error copies the most recent internal failure text into
 *   caller memory and returns the full byte length. Passing cap=0 is a
 *   length query. The bytes are not NUL-terminated.
 * - r9p_front_next_request stages the returned request by request id for
 *   r9p_front_request_copy. Call sequence per request: next_request,
 *   request_copy(request_id), complete_request.
 * - Two host-side request shapes, both drained by the same
 *   next_request/request_copy/complete_request loop:
 *   - register_intake(prefix): a request LIFECYCLE. A client write to
 *     <prefix>/new enqueues a request; complete_request publishes the
 *     result file at <prefix>/<id>/result for a separate reader. Use for
 *     multi-step requests with a durable per-request subtree.
 *   - register_rpc(path): single-fid request/response (factotum rpc
 *     shape). A client opens <path> O_RDWR, writes the request, and reads
 *     the response on the SAME fid; the fid is the session.
 *     complete_request delivers the response to that parked read. Use for
 *     stateless query/response. A subsequent write on the same fid is a
 *     fresh request; clunk discards a pending one. A read before a write,
 *     or after the host abandons the request, errors.
 * - append_event(path) lazily creates a log node on first append.
 *   register_log(path) instead declares an empty log up front, so an
 *   advertised event-stream path is walkable and subscribable from
 *   attach (before any event): a read at offset 0 blocks until the
 *   first append (tail -f), stat reports end offset 0. Use when the
 *   path is published in a manifest a consumer may walk before the
 *   first event exists.
 * - r9p_front_serve_tcp spawns serving threads; r9p_front_stop halts
 *   accepting. Push calls (set, append_event, complete_request) wake any
 *   blocked 9P readers; a blocked read returns empty at the front's wait
 *   timeout (default 30s).
 * - r9p_front_publish_r9p_export is the generic r9p/Vault service
 *   rendezvous helper for embedded hosts. It renders r9p-export.v1 from
 *   the supplied fields, connects to the Vault 9P endpoint, and publishes
 *   the descriptor through /runtime/srv/<service>. If a matching ready
 *   handle already exists it returns ok; if a stale handle exists it is
 *   removed and recreated through the same namespace surface. Authorization
 *   failures are returned as internal failure details via last_error.
 */

#include <stddef.h>
#include <stdint.h>

typedef struct r9p_front r9p_front;

uint32_t r9p_front_abi_version(void);
r9p_front *r9p_front_new(void);
void r9p_front_free(r9p_front *front);

int32_t r9p_front_set(r9p_front *front, const char *path, size_t path_len,
                      const uint8_t *bytes, size_t bytes_len);
int32_t r9p_front_append_event(r9p_front *front, const char *path,
                               size_t path_len, const uint8_t *bytes,
                               size_t bytes_len);
int32_t r9p_front_register_intake(r9p_front *front, const char *prefix,
                                  size_t prefix_len);
int32_t r9p_front_register_rpc(r9p_front *front, const char *path,
                               size_t path_len);
int32_t r9p_front_register_log(r9p_front *front, const char *path,
                               size_t path_len);
int32_t r9p_front_serve_tcp(r9p_front *front, const char *bind,
                            size_t bind_len, uint16_t *port_out);
int32_t r9p_front_next_request(r9p_front *front, uint64_t timeout_ms,
                               uint64_t *id_out, size_t *len_out);
intptr_t r9p_front_request_copy(r9p_front *front, uint64_t request_id,
                                uint8_t *buf, size_t cap);
int32_t r9p_front_complete_request(r9p_front *front, const char *prefix,
                                   size_t prefix_len, uint64_t request_id,
                                   const uint8_t *bytes, size_t bytes_len);
int32_t r9p_front_stop(r9p_front *front);
int32_t r9p_front_publish_r9p_export(
    r9p_front *front, const char *vault_endpoint_bind,
    size_t vault_endpoint_bind_len, const char *vault_uname,
    size_t vault_uname_len, const char *vault_aname, size_t vault_aname_len,
    const char *service_name, size_t service_name_len,
    const char *export_endpoint_bind, size_t export_endpoint_bind_len,
    const char *export_uname, size_t export_uname_len,
    const char *export_aname, size_t export_aname_len,
    const char *exported_root, size_t exported_root_len,
    const char *transport_class, size_t transport_class_len,
    const char *auth, size_t auth_len, const char *protocol,
    size_t protocol_len, const char *local_root_label,
    size_t local_root_label_len, uint32_t pid, uint32_t msize);
intptr_t r9p_front_last_error(r9p_front *front, uint8_t *buf, size_t cap);

#endif
