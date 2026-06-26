use r9p::blocking::Client;
use std::ffi::c_char;

use super::{bytes_arg, clear_last_error, set_last_error, str_arg, FrontAbi, INVALID, OK};

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
