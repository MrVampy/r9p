#ifndef R9P_FRONT_H
#define R9P_FRONT_H

/*
 * r9p front C ABI, version 10.
 *
 * Contract rules:
 * - r9p_front_abi_version() must return 10 before v10-only calls are made.
 *   Hosts that only use the v9 call set may accept either 9 or 10.
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
 *   r9p_front_request_prefix_copy, r9p_front_request_context_copy, and
 *   r9p_front_request_copy. Call sequence per request: next_request,
 *   request_prefix_copy(request_id), request_context_copy(request_id),
 *   request_copy(request_id), then complete_request, complete_write, or
 *   reject_write according to the registered shape. The prefix is the value
 *   to pass to the completion call: the intake prefix for register_intake, or
 *   the registered path for register_rpc and register_write_relay.
 *   request_prefix_copy and request_context_copy with cap=0 return the
 *   required length without copying. request_copy consumes the staged request
 *   bytes, so copy prefix and context first.
 * - r9p_front_set_pushed_file is the v10 public-door push path. It installs
 *   file bytes with brain-owned qid path, qid version, generation,
 *   visibility class, and wake token. The front must serve those qid fields
 *   exactly; it does not increment them locally.
 * - Three host-side request shapes, all drained by the same
 *   next_request/request_copy loop:
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
 *   - register_write_relay(path): synchronous write relay. A client opens
 *     <path> O_WRITE and writes bytes; the Rwrite count is returned only
 *     after the host calls complete_write. reject_write returns the supplied
 *     error text to the writer, and a missing host reports write relay
 *     unavailable after the front wait timeout. Use for write/control
 *     surfaces where enqueueing is not admission.
 * - set_principal_root(principal, root_path) pushes a v9-style wildcard attach
 *   root for a principal. set_principal_root_aname(principal, aname,
 *   root_path) is the v10 admission form: each call admits one aname for the
 *   principal's pushed root. Installing any root switches attach handling to
 *   explicit pushed roots: principals without a row, or with a non-admitted
 *   aname, fail closed at attach. The front does no policy evaluation and
 *   does not derive principal classes.
 * - r9p_front_set_protocol_limits sets the advertised max msize and open
 *   iounit for newly accepted connections. The front validates the msize
 *   against the r9p codec bounds and serves the supplied iounit on open.
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
 *   updated through the same namespace surface without removing the srv file.
 *   Passing service_unit declares host process ownership; when
 *   host_firewall_admission is empty, TCP exports derive
 *   tcp:<export_endpoint_bind>. Authorization failures are returned as
 *   internal failure details via last_error.
 * - r9p_front_maintain_r9p_export performs the same initial publication,
 *   then keeps a cancellable maintainer owned by the front handle. The
 *   maintainer waits on /runtime/srv-wait/<service>/changed-after/<token>
 *   after each successful publication and republishes through /runtime/srv
 *   when Vault reports that the rendezvous changed. Failed publishes or
 *   failed wait-surface reads retry after retry_interval_ms; 0 selects the
 *   library default. r9p_front_reconcile_r9p_exports nudges all maintainers
 *   immediately. r9p_front_stop/free stop all maintainers before releasing
 *   the handle.
 */

#include <stddef.h>
#include <stdint.h>

typedef struct r9p_front r9p_front;

uint32_t r9p_front_abi_version(void);
r9p_front *r9p_front_new(void);
void r9p_front_free(r9p_front *front);

int32_t r9p_front_set(r9p_front *front, const char *path, size_t path_len,
                      const uint8_t *bytes, size_t bytes_len);
int32_t r9p_front_set_pushed_file(
    r9p_front *front, const char *path, size_t path_len, const uint8_t *bytes,
    size_t bytes_len, uint64_t qid_path, uint32_t qid_version,
    uint64_t generation, const char *visibility_class,
    size_t visibility_class_len, const char *wake_token,
    size_t wake_token_len);
int32_t r9p_front_append_event(r9p_front *front, const char *path,
                               size_t path_len, const uint8_t *bytes,
                               size_t bytes_len);
int32_t r9p_front_register_intake(r9p_front *front, const char *prefix,
                                  size_t prefix_len);
int32_t r9p_front_register_rpc(r9p_front *front, const char *path,
                               size_t path_len);
int32_t r9p_front_register_write_relay(r9p_front *front, const char *path,
                                       size_t path_len);
int32_t r9p_front_register_log(r9p_front *front, const char *path,
                               size_t path_len);
int32_t r9p_front_set_principal_root(r9p_front *front,
                                     const char *principal,
                                     size_t principal_len,
                                     const char *root_path,
                                     size_t root_path_len);
int32_t r9p_front_set_principal_root_aname(r9p_front *front,
                                           const char *principal,
                                           size_t principal_len,
                                           const char *aname,
                                           size_t aname_len,
                                           const char *root_path,
                                           size_t root_path_len);
int32_t r9p_front_set_protocol_limits(r9p_front *front, uint32_t max_msize,
                                      uint32_t iounit);
int32_t r9p_front_serve_tcp(r9p_front *front, const char *bind,
                            size_t bind_len, uint16_t *port_out);
int32_t r9p_front_next_request(r9p_front *front, uint64_t timeout_ms,
                               uint64_t *id_out, size_t *len_out);
intptr_t r9p_front_request_copy(r9p_front *front, uint64_t request_id,
                                uint8_t *buf, size_t cap);
intptr_t r9p_front_request_prefix_copy(r9p_front *front, uint64_t request_id,
                                       uint8_t *buf, size_t cap);
intptr_t r9p_front_request_context_copy(r9p_front *front, uint64_t request_id,
                                        uint8_t *buf, size_t cap);
int32_t r9p_front_complete_request(r9p_front *front, const char *prefix,
                                   size_t prefix_len, uint64_t request_id,
                                   const uint8_t *bytes, size_t bytes_len);
int32_t r9p_front_complete_write(r9p_front *front, const char *prefix,
                                 size_t prefix_len, uint64_t request_id,
                                 uint32_t count);
int32_t r9p_front_reject_write(r9p_front *front, const char *prefix,
                               size_t prefix_len, uint64_t request_id,
                               const char *message, size_t message_len);
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
    size_t local_root_label_len, uint32_t pid, uint32_t msize,
    const char *service_unit, size_t service_unit_len,
    const char *host_firewall_admission, size_t host_firewall_admission_len);
int32_t r9p_front_maintain_r9p_export(
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
    size_t local_root_label_len, uint32_t pid, uint32_t msize,
    uint32_t retry_interval_ms, const char *service_unit,
    size_t service_unit_len, const char *host_firewall_admission,
    size_t host_firewall_admission_len);
int32_t r9p_front_reconcile_r9p_exports(r9p_front *front);
intptr_t r9p_front_last_error(r9p_front *front, uint8_t *buf, size_t cap);

#endif
