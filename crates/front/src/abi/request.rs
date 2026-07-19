use crate::RequestContext;
use std::{ffi::c_char, time::Duration};

use super::{
    bytes_arg, clear_last_error, set_last_error, str_arg, FrontAbi, StagedRequest, INTERNAL,
    INVALID, OK, TIMEOUT,
};

fn request_context_lfe(context: &RequestContext) -> Vec<u8> {
    format!(
        "#M(\"version\" \"r9p-front-request-context.v2\" \"principal_id\" \"{}\" \"uname\" \"{}\" \"aname\" \"{}\" \"session_id\" {} \"fid\" {} \"target_path\" \"{}\" \"offset\" {} \"count\" {} \"open_mode\" {} \"pushed_generation\" {})",
        escape_lfe_string(&context.principal_id),
        escape_lfe_string(&context.uname),
        escape_lfe_string(&context.aname),
        context.session_id,
        context.fid,
        escape_lfe_string(&context.target_path),
        context.offset,
        context.count,
        context.open_mode,
        context.pushed_generation,
    )
    .into_bytes()
}

fn escape_lfe_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_next_request(
    handle: *mut FrontAbi,
    timeout_ms: u64,
    id_out: *mut u64,
    len_out: *mut usize,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    if id_out.is_null() || len_out.is_null() {
        return INVALID;
    }
    match abi.front.next_request(Duration::from_millis(timeout_ms)) {
        Ok(Some(request)) => {
            let request_id = request.request_id;
            let request_len = request.bytes.len();
            match abi.staged_requests.lock() {
                Ok(mut requests) => {
                    requests.insert(
                        request_id,
                        StagedRequest {
                            prefix: request.prefix.into_bytes(),
                            bytes: request.bytes,
                            context: request_context_lfe(&request.context),
                        },
                    );
                }
                Err(_) => return INTERNAL,
            }
            unsafe {
                *id_out = request_id;
                *len_out = request_len;
            }
            clear_last_error(abi);
            OK
        }
        Ok(None) => TIMEOUT,
        Err(error) => set_last_error(abi, error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_request_copy(
    handle: *mut FrontAbi,
    request_id: u64,
    buf: *mut u8,
    cap: usize,
) -> isize {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID as isize;
    };
    let Ok(mut requests) = abi.staged_requests.lock() else {
        return INTERNAL as isize;
    };
    let Some(request) = requests.get(&request_id) else {
        return INVALID as isize;
    };
    if request.bytes.len() > cap {
        return INVALID as isize;
    }
    if request.bytes.is_empty() {
        requests.remove(&request_id);
        return 0;
    }
    if buf.is_null() {
        return INVALID as isize;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(request.bytes.as_ptr(), buf, request.bytes.len());
    }
    let len = request.bytes.len();
    requests.remove(&request_id);
    len as isize
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_request_prefix_copy(
    handle: *mut FrontAbi,
    request_id: u64,
    buf: *mut u8,
    cap: usize,
) -> isize {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID as isize;
    };
    let Ok(requests) = abi.staged_requests.lock() else {
        return INTERNAL as isize;
    };
    let Some(request) = requests.get(&request_id) else {
        return INVALID as isize;
    };
    if cap == 0 {
        return request.prefix.len() as isize;
    }
    if buf.is_null() {
        return INVALID as isize;
    }
    if request.prefix.len() > cap {
        return INVALID as isize;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(request.prefix.as_ptr(), buf, request.prefix.len());
    }
    request.prefix.len() as isize
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_request_context_copy(
    handle: *mut FrontAbi,
    request_id: u64,
    buf: *mut u8,
    cap: usize,
) -> isize {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID as isize;
    };
    let Ok(requests) = abi.staged_requests.lock() else {
        return INTERNAL as isize;
    };
    let Some(request) = requests.get(&request_id) else {
        return INVALID as isize;
    };
    if cap == 0 {
        return request.context.len() as isize;
    }
    if buf.is_null() {
        return INVALID as isize;
    }
    if request.context.len() > cap {
        return INVALID as isize;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(request.context.as_ptr(), buf, request.context.len());
    }
    request.context.len() as isize
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_complete_request(
    handle: *mut FrontAbi,
    prefix: *const c_char,
    prefix_len: usize,
    request_id: u64,
    bytes: *const u8,
    bytes_len: usize,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    let (Some(prefix), Some(bytes)) = (unsafe { str_arg(prefix, prefix_len) }, unsafe {
        bytes_arg(bytes, bytes_len)
    }) else {
        return INVALID;
    };
    if let Ok(mut requests) = abi.staged_requests.lock() {
        requests.remove(&request_id);
    }
    match abi.front.complete_request(prefix, request_id, bytes) {
        Ok(()) => {
            clear_last_error(abi);
            OK
        }
        Err(error) => set_last_error(abi, error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_reject_request(
    handle: *mut FrontAbi,
    prefix: *const c_char,
    prefix_len: usize,
    request_id: u64,
    message: *const c_char,
    message_len: usize,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    let (Some(prefix), Some(message)) = (unsafe { str_arg(prefix, prefix_len) }, unsafe {
        str_arg(message, message_len)
    }) else {
        return INVALID;
    };
    if let Ok(mut requests) = abi.staged_requests.lock() {
        requests.remove(&request_id);
    }
    match abi.front.reject_request(prefix, request_id, message) {
        Ok(()) => {
            clear_last_error(abi);
            OK
        }
        Err(error) => set_last_error(abi, error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_complete_write(
    handle: *mut FrontAbi,
    prefix: *const c_char,
    prefix_len: usize,
    request_id: u64,
    count: u32,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    let Some(prefix) = (unsafe { str_arg(prefix, prefix_len) }) else {
        return INVALID;
    };
    if let Ok(mut requests) = abi.staged_requests.lock() {
        requests.remove(&request_id);
    }
    match abi.front.complete_write(prefix, request_id, count) {
        Ok(()) => {
            clear_last_error(abi);
            OK
        }
        Err(error) => set_last_error(abi, error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_reject_write(
    handle: *mut FrontAbi,
    prefix: *const c_char,
    prefix_len: usize,
    request_id: u64,
    message: *const c_char,
    message_len: usize,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    let (Some(prefix), Some(message)) = (unsafe { str_arg(prefix, prefix_len) }, unsafe {
        str_arg(message, message_len)
    }) else {
        return INVALID;
    };
    if let Ok(mut requests) = abi.staged_requests.lock() {
        requests.remove(&request_id);
    }
    match abi.front.reject_write(prefix, request_id, message) {
        Ok(()) => {
            clear_last_error(abi);
            OK
        }
        Err(error) => set_last_error(abi, error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_complete_remove(
    handle: *mut FrontAbi,
    prefix: *const c_char,
    prefix_len: usize,
    request_id: u64,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    let Some(prefix) = (unsafe { str_arg(prefix, prefix_len) }) else {
        return INVALID;
    };
    if let Ok(mut requests) = abi.staged_requests.lock() {
        requests.remove(&request_id);
    }
    match abi.front.complete_remove(prefix, request_id) {
        Ok(()) => {
            clear_last_error(abi);
            OK
        }
        Err(error) => set_last_error(abi, error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_reject_remove(
    handle: *mut FrontAbi,
    prefix: *const c_char,
    prefix_len: usize,
    request_id: u64,
    message: *const c_char,
    message_len: usize,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    let (Some(prefix), Some(message)) = (unsafe { str_arg(prefix, prefix_len) }, unsafe {
        str_arg(message, message_len)
    }) else {
        return INVALID;
    };
    if let Ok(mut requests) = abi.staged_requests.lock() {
        requests.remove(&request_id);
    }
    match abi.front.reject_remove(prefix, request_id, message) {
        Ok(()) => {
            clear_last_error(abi);
            OK
        }
        Err(error) => set_last_error(abi, error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_complete_wstat(
    handle: *mut FrontAbi,
    prefix: *const c_char,
    prefix_len: usize,
    request_id: u64,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    let Some(prefix) = (unsafe { str_arg(prefix, prefix_len) }) else {
        return INVALID;
    };
    if let Ok(mut requests) = abi.staged_requests.lock() {
        requests.remove(&request_id);
    }
    match abi.front.complete_wstat(prefix, request_id) {
        Ok(()) => {
            clear_last_error(abi);
            OK
        }
        Err(error) => set_last_error(abi, error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_reject_wstat(
    handle: *mut FrontAbi,
    prefix: *const c_char,
    prefix_len: usize,
    request_id: u64,
    message: *const c_char,
    message_len: usize,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    let (Some(prefix), Some(message)) = (unsafe { str_arg(prefix, prefix_len) }, unsafe {
        str_arg(message, message_len)
    }) else {
        return INVALID;
    };
    if let Ok(mut requests) = abi.staged_requests.lock() {
        requests.remove(&request_id);
    }
    match abi.front.reject_wstat(prefix, request_id, message) {
        Ok(()) => {
            clear_last_error(abi);
            OK
        }
        Err(error) => set_last_error(abi, error),
    }
}
