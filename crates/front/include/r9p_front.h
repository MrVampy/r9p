#ifndef R9P_FRONT_H
#define R9P_FRONT_H

/*
 * r9p front C ABI, generation 21.
 *
 * Contract rules:
 * - Call r9p_front_abi_version() first and require
 *   R9P_FRONT_ABI_VERSION. ABI generations change only when an existing
 *   signature, lifetime rule, or data contract changes incompatibly.
 * - After the ABI check, call r9p_front_capabilities() and require every
 *   capability used by the consumer. Additive surfaces set a new stable bit
 *   without changing the ABI generation. Unknown bits must be ignored and
 *   published bits are never repurposed.
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
 *   request_copy(request_id), then a completion/rejection call according to
 *   the registered shape. The prefix is the value to pass to the completion
 *   call: the intake prefix for register_intake, or the registered path for
 *   register_rpc, register_read_relay, register_snapshot_read_relay,
 *   register_write_relay,
 *   register_remove_relay, or
 *   register_wstat_relay.
 *   request_prefix_copy and request_context_copy with cap=0 return the
 *   required length without copying. request_copy consumes the staged request
 *   bytes, so copy prefix and context first.
 * - r9p_front_set_pushed_file is the v10 public-door file push path. It
 *   installs file bytes with owner-provided qid path, qid version, generation,
 *   visibility class, freshness reference, and wake token. The front must
 *   serve those qid fields exactly; it does not increment them locally.
 * - r9p_front_set_pushed_directory is the v11 public-door directory push
 *   path. It installs a visible directory with the same owner-provided metadata
 *   contract as pushed files. Use it for admitted roots and visible
 *   intermediate directories; the front must not invent public qid identity.
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
 *   - register_read_relay(path): read-only synthetic file. Each Tread
 *     enqueues one request carrying its offset and count. complete_request
 *     supplies that read's bytes; reject_request returns the supplied 9P
 *     error. The response is consumed by that Tread and is not cached.
 *   - register_snapshot_read_relay(path): read-only finite-record file.
 *     The first Tread on an opened fid enqueues one request.
 *     complete_request supplies the full record, which remains pinned to
 *     that fid and serves all later ranges through explicit EOF. Clunk
 *     retires it. Use for coherent dynamic status and report files.
 *   - register_write_relay(path): synchronous write relay. A client opens
 *     <path> O_WRITE and writes bytes; the Rwrite count is returned only
 *     after the host calls complete_write. reject_write returns the supplied
 *     error text to the writer, and a missing host reports write relay
 *     unavailable after the front wait timeout. Use for write/control
 *     surfaces where enqueueing is not admission.
 *   - register_remove_relay(path): synchronous remove relay. A client
 *     Tremove on path enqueues a request with empty bytes; complete_remove
 *     removes the projected subtree from the front, while reject_remove
 *     returns the supplied error text to the remover.
 *   - register_wstat_relay(path): synchronous wstat relay. A client Twstat
 *     on path enqueues a request whose bytes are the encoded 9P stat payload;
 *     complete_wstat accepts the metadata mutation, while reject_wstat
 *     returns the supplied error text to the caller.
 * - set_principal_root(principal, root_path) pushes a v9-style wildcard attach
 *   root for a principal. set_principal_root_aname(principal, aname,
 *   root_path) admits one aname while using the same value for uname and
 *   principal id. set_principal_class_aname(uname, principal_id, aname,
 *   root_path) is the v10 public-door admission form. Installing any root
 *   switches attach handling to explicit pushed roots: principals without a
 *   row, or with a non-admitted aname, fail closed at attach. The front does
 *   no policy evaluation and does not derive principal classes.
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
 * - r9p_front_serve_tcp_authenticated reads an r9p session-auth server
 *   configuration before binding. Every accepted TCP stream completes p9any
 *   and Noise XX mutual-certificate authentication before 9P negotiation. The
 *   authenticated principal is bound to the 9P attach uname. Use this for any
 *   network front;
 *   plain r9p_front_serve_tcp remains the explicit contained-host boundary.
 * - r9p_front_client_rpc is the v13 outbound 9P RPC helper. It connects to
 *   endpoint_bind, attaches as uname/aname, opens path O_RDWR, writes the
 *   request, reads the response on the same fid, and copies that response into
 *   caller-provided memory. It returns ok only for a complete single-request
 *   exchange. response_len_out is always set to the full response length when
 *   the exchange succeeds, even if response_cap is too small and the call
 *   returns internal failure.
 * - r9p_front_client_read is the v14 outbound 9P read helper. It connects to
 *   endpoint_bind, attaches as uname/aname, opens path O_READ, reads the file
 *   contents, and copies the bytes into caller-provided memory. response_len_out
 *   follows the same full-length contract as r9p_front_client_rpc.
 * - r9p_front_client_create_at, r9p_front_client_create_write_at,
 *   r9p_front_client_write_file, and r9p_front_client_remove are outbound
 *   namespace-client mutation helpers. create_write_at keeps the created fid
 *   open for its initial write; the other operations create, replace, and
 *   remove. All outbound client operations follow 9P2000.R referrals
 *   internally.
 * - r9p_front_set_client_authentication sets the one credential outbound
 *   client sessions authenticate with, and the responder a root dial expects.
 *   Pass an empty responder when the root transport is itself the boundary;
 *   referrals supply their own. The local path is client mechanism and never
 *   appears in the served namespace.
 */

#include <stddef.h>
#include <stdint.h>

#define R9P_FRONT_ABI_VERSION UINT32_C(22)
#define R9P_FRONT_CAP_PUSHED_NAMESPACE_METADATA (UINT64_C(1) << 0)
#define R9P_FRONT_CAP_REQUEST_CONTEXT_V2 (UINT64_C(1) << 1)
#define R9P_FRONT_CAP_SYNTHETIC_READ_RELAY (UINT64_C(1) << 2)
#define R9P_FRONT_CAP_NATIVE_CLIENT_MUTATIONS (UINT64_C(1) << 3)
#define R9P_FRONT_CAP_ATOMIC_CREATE_WRITE (UINT64_C(1) << 4)
#define R9P_FRONT_CAP_NAMESPACE_MUTATION_RELAYS (UINT64_C(1) << 5)
#define R9P_FRONT_CAP_AUTHENTICATED_SERVE (UINT64_C(1) << 6)
#define R9P_FRONT_CAP_CLIENT_SESSION_AUTHENTICATION (UINT64_C(1) << 7)
#define R9P_FRONT_CAP_SNAPSHOT_READ_RELAY (UINT64_C(1) << 8)

typedef struct r9p_front r9p_front;

uint32_t r9p_front_abi_version(void);
uint64_t r9p_front_capabilities(void);
r9p_front *r9p_front_new(void);
void r9p_front_free(r9p_front *front);
int32_t r9p_front_set_client_authentication(
    r9p_front *front, const char *auth_config_path,
    size_t auth_config_path_len, const char *expected_responder,
    size_t expected_responder_len);

int32_t r9p_front_set(r9p_front *front, const char *path, size_t path_len,
                      const uint8_t *bytes, size_t bytes_len);
int32_t r9p_front_set_pushed_file(
    r9p_front *front, const char *path, size_t path_len, const uint8_t *bytes,
    size_t bytes_len, uint64_t qid_path, uint32_t qid_version,
    uint64_t generation, const char *visibility_class,
    size_t visibility_class_len, const char *freshness_ref,
    size_t freshness_ref_len, const char *wake_token, size_t wake_token_len);
int32_t r9p_front_set_pushed_directory(
    r9p_front *front, const char *path, size_t path_len, uint64_t qid_path,
    uint32_t qid_version, uint64_t generation, const char *visibility_class,
    size_t visibility_class_len, const char *freshness_ref,
    size_t freshness_ref_len, const char *wake_token, size_t wake_token_len);
int32_t r9p_front_append_event(r9p_front *front, const char *path,
                               size_t path_len, const uint8_t *bytes,
                               size_t bytes_len);
int32_t r9p_front_register_intake(r9p_front *front, const char *prefix,
                                  size_t prefix_len);
int32_t r9p_front_register_rpc(r9p_front *front, const char *path,
                               size_t path_len);
int32_t r9p_front_register_read_relay(r9p_front *front, const char *path,
                                      size_t path_len);
int32_t r9p_front_register_snapshot_read_relay(
    r9p_front *front, const char *path, size_t path_len);
int32_t r9p_front_register_write_relay(r9p_front *front, const char *path,
                                       size_t path_len);
int32_t r9p_front_register_remove_relay(r9p_front *front, const char *path,
                                        size_t path_len);
int32_t r9p_front_register_wstat_relay(r9p_front *front, const char *path,
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
int32_t r9p_front_set_principal_class_aname(
    r9p_front *front, const char *uname, size_t uname_len,
    const char *principal_id, size_t principal_id_len, const char *aname,
    size_t aname_len, const char *root_path, size_t root_path_len);
int32_t r9p_front_set_protocol_limits(r9p_front *front, uint32_t max_msize,
                                      uint32_t iounit);
int32_t r9p_front_serve_tcp(r9p_front *front, const char *bind,
                            size_t bind_len, uint16_t *port_out);
int32_t r9p_front_serve_tcp_authenticated(
    r9p_front *front, const char *bind, size_t bind_len,
    const char *auth_config_path, size_t auth_config_path_len,
    uint16_t *port_out);
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
int32_t r9p_front_reject_request(r9p_front *front, const char *prefix,
                                 size_t prefix_len, uint64_t request_id,
                                 const char *message, size_t message_len);
int32_t r9p_front_complete_write(r9p_front *front, const char *prefix,
                                 size_t prefix_len, uint64_t request_id,
                                 uint32_t count);
int32_t r9p_front_reject_write(r9p_front *front, const char *prefix,
                               size_t prefix_len, uint64_t request_id,
                               const char *message, size_t message_len);
int32_t r9p_front_complete_remove(r9p_front *front, const char *prefix,
                                  size_t prefix_len, uint64_t request_id);
int32_t r9p_front_reject_remove(r9p_front *front, const char *prefix,
                                size_t prefix_len, uint64_t request_id,
                                const char *message, size_t message_len);
int32_t r9p_front_complete_wstat(r9p_front *front, const char *prefix,
                                 size_t prefix_len, uint64_t request_id);
int32_t r9p_front_reject_wstat(r9p_front *front, const char *prefix,
                               size_t prefix_len, uint64_t request_id,
                               const char *message, size_t message_len);
int32_t r9p_front_stop(r9p_front *front);
int32_t r9p_front_client_rpc(
    r9p_front *front, const char *endpoint_bind, size_t endpoint_bind_len,
    const char *uname, size_t uname_len, const char *aname, size_t aname_len,
    const char *path, size_t path_len, const uint8_t *request,
    size_t request_len, uint32_t msize, uint8_t *response_out,
    size_t response_cap, size_t *response_len_out);
int32_t r9p_front_client_read(
    r9p_front *front, const char *endpoint_bind, size_t endpoint_bind_len,
    const char *uname, size_t uname_len, const char *aname, size_t aname_len,
    const char *path, size_t path_len, uint32_t msize, uint8_t *response_out,
    size_t response_cap, size_t *response_len_out);
int32_t r9p_front_client_create_at(
    r9p_front *front, const char *endpoint_bind, size_t endpoint_bind_len,
    const char *uname, size_t uname_len, const char *aname, size_t aname_len,
    const char *parent, size_t parent_len, const char *name, size_t name_len,
    uint32_t perm, uint8_t mode, uint32_t msize, uint8_t *qid_type_out,
    uint32_t *qid_version_out, uint64_t *qid_path_out);
int32_t r9p_front_client_create_write_at(
    r9p_front *front, const char *endpoint_bind, size_t endpoint_bind_len,
    const char *uname, size_t uname_len, const char *aname, size_t aname_len,
    const char *parent, size_t parent_len, const char *name, size_t name_len,
    uint32_t perm, uint8_t mode, uint64_t offset, const uint8_t *bytes,
    size_t bytes_len, uint32_t msize, uint32_t *count_out);
int32_t r9p_front_client_write_file(
    r9p_front *front, const char *endpoint_bind, size_t endpoint_bind_len,
    const char *uname, size_t uname_len, const char *aname, size_t aname_len,
    const char *path, size_t path_len, const uint8_t *bytes, size_t bytes_len,
    uint32_t msize, uint32_t *count_out);
int32_t r9p_front_client_remove(
    r9p_front *front, const char *endpoint_bind, size_t endpoint_bind_len,
    const char *uname, size_t uname_len, const char *aname, size_t aname_len,
    const char *path, size_t path_len, uint32_t msize);
intptr_t r9p_front_last_error(r9p_front *front, uint8_t *buf, size_t cap);

#endif
