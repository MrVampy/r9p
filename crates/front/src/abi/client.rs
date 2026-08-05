use session::{Client, ConnectionConfig};
use std::{ffi::c_char, path::PathBuf, time::Duration};

use super::{bytes_arg, clear_last_error, set_last_error, str_arg, FrontAbi, INVALID, OK};

const CLIENT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

#[no_mangle]
pub unsafe extern "C" fn r9p_front_bind_client_authority(
    handle: *mut FrontAbi,
    authority_boundary: *const c_char,
    authority_boundary_len: usize,
    auth_config_path: *const c_char,
    auth_config_path_len: usize,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    let (Some(authority_boundary), Some(auth_config_path)) = (
        unsafe { str_arg(authority_boundary, authority_boundary_len) },
        unsafe { str_arg(auth_config_path, auth_config_path_len) },
    ) else {
        return INVALID;
    };
    let mut authorities = match abi.client_authorities.lock() {
        Ok(authorities) => authorities,
        Err(error) => return set_last_error(abi, error),
    };
    match authorities.insert_session_auth(authority_boundary, PathBuf::from(auth_config_path)) {
        Ok(()) => {
            clear_last_error(abi);
            OK
        }
        Err(error) => set_last_error(abi, error),
    }
}

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
    let client = match connect_client(abi, endpoint_bind, uname, aname, msize) {
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
    let client = match connect_client(abi, endpoint_bind, uname, aname, msize) {
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
    let client = match connect_client(abi, endpoint_bind, uname, aname, msize) {
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
    let client = match connect_client(abi, endpoint_bind, uname, aname, msize) {
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
    let client = match connect_client(abi, endpoint_bind, uname, aname, msize) {
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
    let client = match connect_client(abi, endpoint_bind, uname, aname, msize) {
        Ok(client) => client,
        Err(error) => return set_last_error(abi, error),
    };
    if let Err(error) = client.remove_path(path) {
        return set_last_error(abi, error);
    }
    clear_last_error(abi);
    OK
}

fn connect_client(
    abi: &FrontAbi,
    endpoint_bind: &str,
    uname: &str,
    aname: &str,
    msize: u32,
) -> session::Result<Client> {
    let authorities = abi
        .client_authorities
        .lock()
        .map_err(|_| session::Error::new(libc::EIO, "front client authority lock poisoned"))?
        .clone();
    Client::connect_with_timeout(
        &ConnectionConfig {
            auth_domain: None,
            address: endpoint_bind.to_string(),
            uname: uname.to_string(),
            aname: aname.to_string(),
            msize,
            auth_config: None,
            authorities,
        },
        CLIENT_CONNECT_TIMEOUT,
    )
}
