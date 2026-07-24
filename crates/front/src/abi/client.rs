use r9p::blocking::Client;
use session::{AuthorityBindings, Client as SessionClient, ConnectionConfig};
use std::{ffi::c_char, path::PathBuf, time::Duration};

use super::{bytes_arg, clear_last_error, set_last_error, str_arg, FrontAbi, INVALID, OK};

const MAX_RESOLVED_RESPONSE_BYTES: u32 = 4 * 1024 * 1024;

#[no_mangle]
pub unsafe extern "C" fn r9p_front_client_rpc(
    handle: *mut FrontAbi,
    endpoint_bind: *const c_char,
    endpoint_bind_len: usize,
    uname: *const c_char,
    uname_len: usize,
    aname: *const c_char,
    aname_len: usize,
    path: *const c_char,
    path_len: usize,
    request: *const u8,
    request_len: usize,
    msize: u32,
    response_out: *mut u8,
    response_cap: usize,
    response_len_out: *mut usize,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    if response_len_out.is_null() {
        return INVALID;
    }
    let (Some(endpoint_bind), Some(uname), Some(aname), Some(path), Some(request)) = (
        unsafe { str_arg(endpoint_bind, endpoint_bind_len) },
        unsafe { str_arg(uname, uname_len) },
        unsafe { str_arg(aname, aname_len) },
        unsafe { str_arg(path, path_len) },
        unsafe { bytes_arg(request, request_len) },
    ) else {
        return INVALID;
    };
    let mut client = match Client::connect_tcp(endpoint_bind, uname, aname, msize) {
        Ok(client) => client,
        Err(error) => return set_last_error(abi, error),
    };
    let response = match client.rpc_path(path, request) {
        Ok(response) => response,
        Err(error) => return set_last_error(abi, error),
    };
    unsafe {
        *response_len_out = response.len();
    }
    if response.len() > response_cap {
        return set_last_error(
            abi,
            format!(
                "client rpc response too large: response_len={} response_cap={response_cap}",
                response.len()
            ),
        );
    }
    if !response.is_empty() {
        if response_out.is_null() {
            return INVALID;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(response.as_ptr(), response_out, response.len());
        }
    }
    clear_last_error(abi);
    OK
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_client_read(
    handle: *mut FrontAbi,
    endpoint_bind: *const c_char,
    endpoint_bind_len: usize,
    uname: *const c_char,
    uname_len: usize,
    aname: *const c_char,
    aname_len: usize,
    path: *const c_char,
    path_len: usize,
    msize: u32,
    response_out: *mut u8,
    response_cap: usize,
    response_len_out: *mut usize,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    if response_len_out.is_null() {
        return INVALID;
    }
    let (Some(endpoint_bind), Some(uname), Some(aname), Some(path)) = (
        unsafe { str_arg(endpoint_bind, endpoint_bind_len) },
        unsafe { str_arg(uname, uname_len) },
        unsafe { str_arg(aname, aname_len) },
        unsafe { str_arg(path, path_len) },
    ) else {
        return INVALID;
    };
    let mut client = match Client::connect_tcp(endpoint_bind, uname, aname, msize) {
        Ok(client) => client,
        Err(error) => return set_last_error(abi, error),
    };
    let response = match client.read_path(path) {
        Ok(response) => response,
        Err(error) => return set_last_error(abi, error),
    };
    unsafe {
        *response_len_out = response.len();
    }
    if response.len() > response_cap {
        return set_last_error(
            abi,
            format!(
                "client read response too large: response_len={} response_cap={response_cap}",
                response.len()
            ),
        );
    }
    if !response.is_empty() {
        if response_out.is_null() {
            return INVALID;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(response.as_ptr(), response_out, response.len());
        }
    }
    clear_last_error(abi);
    OK
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_client_resolved_rpc(
    handle: *mut FrontAbi,
    resolver_bind: *const c_char,
    resolver_bind_len: usize,
    resolver_uname: *const c_char,
    resolver_uname_len: usize,
    resolver_aname: *const c_char,
    resolver_aname_len: usize,
    resolver_auth_config: *const c_char,
    resolver_auth_config_len: usize,
    namespace_path: *const c_char,
    namespace_path_len: usize,
    authority_boundary: *const c_char,
    authority_boundary_len: usize,
    service_auth_config: *const c_char,
    service_auth_config_len: usize,
    request: *const u8,
    request_len: usize,
    msize: u32,
    timeout_ms: u64,
    response_out: *mut u8,
    response_cap: usize,
    response_len_out: *mut usize,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    if response_len_out.is_null() {
        return INVALID;
    }
    let (
        Some(resolver_bind),
        Some(resolver_uname),
        Some(resolver_aname),
        Some(resolver_auth_config),
        Some(namespace_path),
        Some(authority_boundary),
        Some(service_auth_config),
        Some(request),
    ) = (
        unsafe { str_arg(resolver_bind, resolver_bind_len) },
        unsafe { str_arg(resolver_uname, resolver_uname_len) },
        unsafe { str_arg(resolver_aname, resolver_aname_len) },
        unsafe { str_arg(resolver_auth_config, resolver_auth_config_len) },
        unsafe { str_arg(namespace_path, namespace_path_len) },
        unsafe { str_arg(authority_boundary, authority_boundary_len) },
        unsafe { str_arg(service_auth_config, service_auth_config_len) },
        unsafe { bytes_arg(request, request_len) },
    )
    else {
        return INVALID;
    };
    let response = match resolved_rpc(
        resolver_bind,
        resolver_uname,
        resolver_aname,
        resolver_auth_config,
        namespace_path,
        authority_boundary,
        service_auth_config,
        request,
        msize,
        timeout_ms,
    ) {
        Ok(response) => response,
        Err(error) => return set_last_error(abi, error),
    };
    copy_response(
        abi,
        &response,
        response_out,
        response_cap,
        response_len_out,
        "resolved rpc",
    )
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_client_resolved_read(
    handle: *mut FrontAbi,
    resolver_bind: *const c_char,
    resolver_bind_len: usize,
    resolver_uname: *const c_char,
    resolver_uname_len: usize,
    resolver_aname: *const c_char,
    resolver_aname_len: usize,
    resolver_auth_config: *const c_char,
    resolver_auth_config_len: usize,
    namespace_path: *const c_char,
    namespace_path_len: usize,
    authority_boundary: *const c_char,
    authority_boundary_len: usize,
    service_auth_config: *const c_char,
    service_auth_config_len: usize,
    msize: u32,
    timeout_ms: u64,
    response_out: *mut u8,
    response_cap: usize,
    response_len_out: *mut usize,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    if response_len_out.is_null() {
        return INVALID;
    }
    let (
        Some(resolver_bind),
        Some(resolver_uname),
        Some(resolver_aname),
        Some(resolver_auth_config),
        Some(namespace_path),
        Some(authority_boundary),
        Some(service_auth_config),
    ) = (
        unsafe { str_arg(resolver_bind, resolver_bind_len) },
        unsafe { str_arg(resolver_uname, resolver_uname_len) },
        unsafe { str_arg(resolver_aname, resolver_aname_len) },
        unsafe { str_arg(resolver_auth_config, resolver_auth_config_len) },
        unsafe { str_arg(namespace_path, namespace_path_len) },
        unsafe { str_arg(authority_boundary, authority_boundary_len) },
        unsafe { str_arg(service_auth_config, service_auth_config_len) },
    )
    else {
        return INVALID;
    };
    let response = match resolved_read(
        resolver_bind,
        resolver_uname,
        resolver_aname,
        resolver_auth_config,
        namespace_path,
        authority_boundary,
        service_auth_config,
        msize,
        timeout_ms,
    ) {
        Ok(response) => response,
        Err(error) => return set_last_error(abi, error),
    };
    copy_response(
        abi,
        &response,
        response_out,
        response_cap,
        response_len_out,
        "resolved read",
    )
}

#[allow(clippy::too_many_arguments)]
fn resolved_rpc(
    resolver_bind: &str,
    resolver_uname: &str,
    resolver_aname: &str,
    resolver_auth_config: &str,
    namespace_path: &str,
    authority_boundary: &str,
    service_auth_config: &str,
    request: &[u8],
    msize: u32,
    timeout_ms: u64,
) -> session::Result<Vec<u8>> {
    let timeout = resolved_timeout(timeout_ms)?;
    let resolver = SessionClient::connect_with_timeout(
        &resolver_config(
            resolver_bind,
            resolver_uname,
            resolver_aname,
            resolver_auth_config,
            msize,
        ),
        timeout,
    )?;
    let authorities = authority_bindings(authority_boundary, service_auth_config)?;
    let resolved =
        resolver.resolve_namespace_path_timeout(namespace_path, msize, &authorities, timeout)?;
    let response = resolved.rpc_timeout(request, timeout, timeout, MAX_RESOLVED_RESPONSE_BYTES)?;
    resolver.shutdown()?;
    Ok(response)
}

#[allow(clippy::too_many_arguments)]
fn resolved_read(
    resolver_bind: &str,
    resolver_uname: &str,
    resolver_aname: &str,
    resolver_auth_config: &str,
    namespace_path: &str,
    authority_boundary: &str,
    service_auth_config: &str,
    msize: u32,
    timeout_ms: u64,
) -> session::Result<Vec<u8>> {
    let timeout = resolved_timeout(timeout_ms)?;
    let resolver = SessionClient::connect_with_timeout(
        &resolver_config(
            resolver_bind,
            resolver_uname,
            resolver_aname,
            resolver_auth_config,
            msize,
        ),
        timeout,
    )?;
    let authorities = authority_bindings(authority_boundary, service_auth_config)?;
    let resolved =
        resolver.resolve_namespace_path_timeout(namespace_path, msize, &authorities, timeout)?;
    let response = resolved.read_timeout(timeout, timeout, MAX_RESOLVED_RESPONSE_BYTES)?;
    resolver.shutdown()?;
    Ok(response)
}

fn resolver_config(
    bind: &str,
    uname: &str,
    aname: &str,
    auth_config: &str,
    msize: u32,
) -> ConnectionConfig {
    ConnectionConfig {
        address: bind.to_string(),
        uname: uname.to_string(),
        aname: aname.to_string(),
        msize,
        auth_config: optional_path(auth_config),
    }
}

fn authority_bindings(
    authority_boundary: &str,
    service_auth_config: &str,
) -> session::Result<AuthorityBindings> {
    match (
        authority_boundary.is_empty(),
        service_auth_config.is_empty(),
    ) {
        (true, true) => Ok(AuthorityBindings::new()),
        (false, false) => AuthorityBindings::new()
            .bind_session_auth(authority_boundary, PathBuf::from(service_auth_config)),
        _ => Err(session::Error::new(
            libc::EINVAL,
            "resolved client authority boundary and auth config must be supplied together",
        )),
    }
}

fn optional_path(path: &str) -> Option<PathBuf> {
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

fn resolved_timeout(timeout_ms: u64) -> session::Result<Duration> {
    let timeout = Duration::from_millis(timeout_ms);
    if timeout.is_zero() {
        Err(session::Error::new(
            libc::EINVAL,
            "resolved client timeout must be nonzero",
        ))
    } else {
        Ok(timeout)
    }
}

fn copy_response(
    abi: &FrontAbi,
    response: &[u8],
    response_out: *mut u8,
    response_cap: usize,
    response_len_out: *mut usize,
    operation: &str,
) -> i32 {
    unsafe {
        *response_len_out = response.len();
    }
    if response.len() > response_cap {
        return set_last_error(
            abi,
            format!(
                "client {operation} response too large: response_len={} response_cap={response_cap}",
                response.len()
            ),
        );
    }
    if !response.is_empty() {
        if response_out.is_null() {
            return INVALID;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(response.as_ptr(), response_out, response.len());
        }
    }
    clear_last_error(abi);
    OK
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_client_create_at(
    handle: *mut FrontAbi,
    endpoint_bind: *const c_char,
    endpoint_bind_len: usize,
    uname: *const c_char,
    uname_len: usize,
    aname: *const c_char,
    aname_len: usize,
    parent: *const c_char,
    parent_len: usize,
    name: *const c_char,
    name_len: usize,
    perm: u32,
    mode: u8,
    msize: u32,
    qid_type_out: *mut u8,
    qid_version_out: *mut u32,
    qid_path_out: *mut u64,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    if qid_type_out.is_null() || qid_version_out.is_null() || qid_path_out.is_null() {
        return INVALID;
    }
    let (Some(endpoint_bind), Some(uname), Some(aname), Some(parent), Some(name)) = (
        unsafe { str_arg(endpoint_bind, endpoint_bind_len) },
        unsafe { str_arg(uname, uname_len) },
        unsafe { str_arg(aname, aname_len) },
        unsafe { str_arg(parent, parent_len) },
        unsafe { str_arg(name, name_len) },
    ) else {
        return INVALID;
    };
    let mut client = match Client::connect_tcp(endpoint_bind, uname, aname, msize) {
        Ok(client) => client,
        Err(error) => return set_last_error(abi, error),
    };
    let qid = match client.create_at(parent, name, perm, mode) {
        Ok(qid) => qid,
        Err(error) => return set_last_error(abi, error),
    };
    unsafe {
        *qid_type_out = qid.qtype;
        *qid_version_out = qid.version;
        *qid_path_out = qid.path;
    }
    clear_last_error(abi);
    OK
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_client_create_write_at(
    handle: *mut FrontAbi,
    endpoint_bind: *const c_char,
    endpoint_bind_len: usize,
    uname: *const c_char,
    uname_len: usize,
    aname: *const c_char,
    aname_len: usize,
    parent: *const c_char,
    parent_len: usize,
    name: *const c_char,
    name_len: usize,
    perm: u32,
    mode: u8,
    offset: u64,
    bytes: *const u8,
    bytes_len: usize,
    msize: u32,
    count_out: *mut u32,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    if count_out.is_null() {
        return INVALID;
    }
    let (Some(endpoint_bind), Some(uname), Some(aname), Some(parent), Some(name), Some(bytes)) = (
        unsafe { str_arg(endpoint_bind, endpoint_bind_len) },
        unsafe { str_arg(uname, uname_len) },
        unsafe { str_arg(aname, aname_len) },
        unsafe { str_arg(parent, parent_len) },
        unsafe { str_arg(name, name_len) },
        unsafe { bytes_arg(bytes, bytes_len) },
    ) else {
        return INVALID;
    };
    let mut client = match Client::connect_tcp(endpoint_bind, uname, aname, msize) {
        Ok(client) => client,
        Err(error) => return set_last_error(abi, error),
    };
    let count = match client.create_write_at(parent, name, perm, mode, offset, bytes) {
        Ok(count) => count,
        Err(error) => return set_last_error(abi, error),
    };
    unsafe {
        *count_out = count;
    }
    clear_last_error(abi);
    OK
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_client_write_file(
    handle: *mut FrontAbi,
    endpoint_bind: *const c_char,
    endpoint_bind_len: usize,
    uname: *const c_char,
    uname_len: usize,
    aname: *const c_char,
    aname_len: usize,
    path: *const c_char,
    path_len: usize,
    bytes: *const u8,
    bytes_len: usize,
    msize: u32,
    count_out: *mut u32,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    if count_out.is_null() {
        return INVALID;
    }
    let (Some(endpoint_bind), Some(uname), Some(aname), Some(path), Some(bytes)) = (
        unsafe { str_arg(endpoint_bind, endpoint_bind_len) },
        unsafe { str_arg(uname, uname_len) },
        unsafe { str_arg(aname, aname_len) },
        unsafe { str_arg(path, path_len) },
        unsafe { bytes_arg(bytes, bytes_len) },
    ) else {
        return INVALID;
    };
    let mut client = match Client::connect_tcp(endpoint_bind, uname, aname, msize) {
        Ok(client) => client,
        Err(error) => return set_last_error(abi, error),
    };
    let count = match client.write_file(path, bytes) {
        Ok(count) => count,
        Err(error) => return set_last_error(abi, error),
    };
    unsafe {
        *count_out = count;
    }
    clear_last_error(abi);
    OK
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_client_remove(
    handle: *mut FrontAbi,
    endpoint_bind: *const c_char,
    endpoint_bind_len: usize,
    uname: *const c_char,
    uname_len: usize,
    aname: *const c_char,
    aname_len: usize,
    path: *const c_char,
    path_len: usize,
    msize: u32,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    let (Some(endpoint_bind), Some(uname), Some(aname), Some(path)) = (
        unsafe { str_arg(endpoint_bind, endpoint_bind_len) },
        unsafe { str_arg(uname, uname_len) },
        unsafe { str_arg(aname, aname_len) },
        unsafe { str_arg(path, path_len) },
    ) else {
        return INVALID;
    };
    let mut client = match Client::connect_tcp(endpoint_bind, uname, aname, msize) {
        Ok(client) => client,
        Err(error) => return set_last_error(abi, error),
    };
    if let Err(error) = client.remove_path(path) {
        return set_last_error(abi, error);
    }
    clear_last_error(abi);
    OK
}
