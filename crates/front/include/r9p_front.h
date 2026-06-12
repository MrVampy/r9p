#ifndef R9P_FRONT_H
#define R9P_FRONT_H

/*
 * r9p front C ABI, version 2.
 *
 * Contract rules:
 * - r9p_front_abi_version() must return 2 before any other call is made;
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
 * - r9p_front_next_request stages the returned request by request id for
 *   r9p_front_request_copy. Call sequence per request: next_request,
 *   request_copy(request_id), complete_request.
 * - r9p_front_serve_tcp spawns serving threads; r9p_front_stop halts
 *   accepting. Push calls (set, append_event, complete_request) wake any
 *   blocked 9P readers; a blocked read returns empty at the front's wait
 *   timeout (default 30s).
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

#endif
