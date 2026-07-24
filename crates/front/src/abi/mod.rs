#![allow(clippy::missing_safety_doc)]

use crate::serve::ServeHandle;
use crate::{Front, PushedDirectoryMetadata, PushedFileMetadata};
use std::collections::BTreeMap;
use std::ffi::c_char;
use std::sync::Mutex;

mod client;
mod request;

pub use client::{
    r9p_front_client_create_at, r9p_front_client_create_write_at, r9p_front_client_read,
    r9p_front_client_remove, r9p_front_client_resolved_read, r9p_front_client_resolved_rpc,
    r9p_front_client_rpc, r9p_front_client_write_file,
};
pub use request::{
    r9p_front_complete_remove, r9p_front_complete_request, r9p_front_complete_write,
    r9p_front_complete_wstat, r9p_front_next_request, r9p_front_reject_remove,
    r9p_front_reject_request, r9p_front_reject_write, r9p_front_reject_wstat,
    r9p_front_request_context_copy, r9p_front_request_copy, r9p_front_request_prefix_copy,
};
pub const ABI_VERSION: u32 = 20;
pub const CAPABILITY_PUSHED_NAMESPACE_METADATA: u64 = 1 << 0;
pub const CAPABILITY_REQUEST_CONTEXT_V2: u64 = 1 << 1;
pub const CAPABILITY_SYNTHETIC_READ_RELAY: u64 = 1 << 2;
pub const CAPABILITY_NATIVE_CLIENT_MUTATIONS: u64 = 1 << 3;
pub const CAPABILITY_ATOMIC_CREATE_WRITE: u64 = 1 << 4;
pub const CAPABILITY_NAMESPACE_MUTATION_RELAYS: u64 = 1 << 5;
pub const CAPABILITY_RESOLVED_NAMESPACE_CLIENT: u64 = 1 << 6;
pub const CAPABILITIES: u64 = CAPABILITY_PUSHED_NAMESPACE_METADATA
    | CAPABILITY_REQUEST_CONTEXT_V2
    | CAPABILITY_SYNTHETIC_READ_RELAY
    | CAPABILITY_NATIVE_CLIENT_MUTATIONS
    | CAPABILITY_ATOMIC_CREATE_WRITE
    | CAPABILITY_NAMESPACE_MUTATION_RELAYS
    | CAPABILITY_RESOLVED_NAMESPACE_CLIENT;

const OK: i32 = 0;
const TIMEOUT: i32 = 1;
const INVALID: i32 = -1;
const INTERNAL: i32 = -2;

pub struct FrontAbi {
    front: Front,
    serves: Mutex<Vec<ServeHandle>>,
    staged_requests: Mutex<BTreeMap<u64, StagedRequest>>,
    last_error: Mutex<Vec<u8>>,
}

struct StagedRequest {
    prefix: Vec<u8>,
    bytes: Vec<u8>,
    context: Vec<u8>,
}

unsafe fn str_arg<'a>(ptr: *const c_char, len: usize) -> Option<&'a str> {
    if ptr.is_null() && len > 0 {
        return None;
    }
    if len == 0 {
        return Some("");
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) };
    std::str::from_utf8(bytes).ok()
}

unsafe fn bytes_arg<'a>(ptr: *const u8, len: usize) -> Option<&'a [u8]> {
    if ptr.is_null() && len > 0 {
        return None;
    }
    if len == 0 {
        return Some(&[]);
    }
    Some(unsafe { std::slice::from_raw_parts(ptr, len) })
}

fn set_last_error(abi: &FrontAbi, error: impl ToString) -> i32 {
    if let Ok(mut last_error) = abi.last_error.lock() {
        *last_error = error.to_string().into_bytes();
    }
    INTERNAL
}

fn clear_last_error(abi: &FrontAbi) {
    if let Ok(mut last_error) = abi.last_error.lock() {
        last_error.clear();
    }
}

#[no_mangle]
pub extern "C" fn r9p_front_abi_version() -> u32 {
    ABI_VERSION
}

#[no_mangle]
pub extern "C" fn r9p_front_capabilities() -> u64 {
    CAPABILITIES
}

#[no_mangle]
pub extern "C" fn r9p_front_new() -> *mut FrontAbi {
    Box::into_raw(Box::new(FrontAbi {
        front: Front::new(),
        serves: Mutex::new(Vec::new()),
        staged_requests: Mutex::new(BTreeMap::new()),
        last_error: Mutex::new(Vec::new()),
    }))
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_free(handle: *mut FrontAbi) {
    if handle.is_null() {
        return;
    }
    let abi = unsafe { Box::from_raw(handle) };
    if let Ok(serves) = abi.serves.lock() {
        for serve in serves.iter() {
            serve.shutdown();
        }
        drop(serves);
    }
    drop(abi);
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_set(
    handle: *mut FrontAbi,
    path: *const c_char,
    path_len: usize,
    bytes: *const u8,
    bytes_len: usize,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    let (Some(path), Some(bytes)) = (unsafe { str_arg(path, path_len) }, unsafe {
        bytes_arg(bytes, bytes_len)
    }) else {
        return INVALID;
    };
    match abi.front.set(path, bytes) {
        Ok(()) => {
            clear_last_error(abi);
            OK
        }
        Err(error) => set_last_error(abi, error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_set_pushed_file(
    handle: *mut FrontAbi,
    path: *const c_char,
    path_len: usize,
    bytes: *const u8,
    bytes_len: usize,
    qid_path: u64,
    qid_version: u32,
    generation: u64,
    visibility_class: *const c_char,
    visibility_class_len: usize,
    freshness_ref: *const c_char,
    freshness_ref_len: usize,
    wake_token: *const c_char,
    wake_token_len: usize,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    let (Some(path), Some(bytes), Some(visibility_class), Some(freshness_ref), Some(wake_token)) = (
        unsafe { str_arg(path, path_len) },
        unsafe { bytes_arg(bytes, bytes_len) },
        unsafe { str_arg(visibility_class, visibility_class_len) },
        unsafe { str_arg(freshness_ref, freshness_ref_len) },
        unsafe { str_arg(wake_token, wake_token_len) },
    ) else {
        return INVALID;
    };
    match abi.front.set_pushed_file(
        path,
        bytes,
        PushedFileMetadata {
            qid_path,
            qid_version,
            generation,
            visibility_class: visibility_class.to_string(),
            freshness_ref: freshness_ref.to_string(),
            wake_token: wake_token.to_string(),
        },
    ) {
        Ok(()) => {
            clear_last_error(abi);
            OK
        }
        Err(error) => set_last_error(abi, error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_set_pushed_directory(
    handle: *mut FrontAbi,
    path: *const c_char,
    path_len: usize,
    qid_path: u64,
    qid_version: u32,
    generation: u64,
    visibility_class: *const c_char,
    visibility_class_len: usize,
    freshness_ref: *const c_char,
    freshness_ref_len: usize,
    wake_token: *const c_char,
    wake_token_len: usize,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    let (Some(path), Some(visibility_class), Some(freshness_ref), Some(wake_token)) = (
        unsafe { str_arg(path, path_len) },
        unsafe { str_arg(visibility_class, visibility_class_len) },
        unsafe { str_arg(freshness_ref, freshness_ref_len) },
        unsafe { str_arg(wake_token, wake_token_len) },
    ) else {
        return INVALID;
    };
    match abi.front.set_pushed_directory(
        path,
        PushedDirectoryMetadata {
            qid_path,
            qid_version,
            generation,
            visibility_class: visibility_class.to_string(),
            freshness_ref: freshness_ref.to_string(),
            wake_token: wake_token.to_string(),
        },
    ) {
        Ok(()) => {
            clear_last_error(abi);
            OK
        }
        Err(error) => set_last_error(abi, error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_append_event(
    handle: *mut FrontAbi,
    path: *const c_char,
    path_len: usize,
    bytes: *const u8,
    bytes_len: usize,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    let (Some(path), Some(bytes)) = (unsafe { str_arg(path, path_len) }, unsafe {
        bytes_arg(bytes, bytes_len)
    }) else {
        return INVALID;
    };
    match abi.front.append_event(path, bytes) {
        Ok(()) => {
            clear_last_error(abi);
            OK
        }
        Err(error) => set_last_error(abi, error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_register_intake(
    handle: *mut FrontAbi,
    prefix: *const c_char,
    prefix_len: usize,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    let Some(prefix) = (unsafe { str_arg(prefix, prefix_len) }) else {
        return INVALID;
    };
    match abi.front.register_intake(prefix) {
        Ok(()) => {
            clear_last_error(abi);
            OK
        }
        Err(error) => set_last_error(abi, error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_register_rpc(
    handle: *mut FrontAbi,
    path: *const c_char,
    path_len: usize,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    let Some(path) = (unsafe { str_arg(path, path_len) }) else {
        return INVALID;
    };
    match abi.front.register_rpc(path) {
        Ok(()) => {
            clear_last_error(abi);
            OK
        }
        Err(error) => set_last_error(abi, error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_register_read_relay(
    handle: *mut FrontAbi,
    path: *const c_char,
    path_len: usize,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    let Some(path) = (unsafe { str_arg(path, path_len) }) else {
        return INVALID;
    };
    match abi.front.register_read_relay(path) {
        Ok(()) => {
            clear_last_error(abi);
            OK
        }
        Err(error) => set_last_error(abi, error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_register_write_relay(
    handle: *mut FrontAbi,
    path: *const c_char,
    path_len: usize,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    let Some(path) = (unsafe { str_arg(path, path_len) }) else {
        return INVALID;
    };
    match abi.front.register_write_relay(path) {
        Ok(()) => {
            clear_last_error(abi);
            OK
        }
        Err(error) => set_last_error(abi, error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_register_remove_relay(
    handle: *mut FrontAbi,
    path: *const c_char,
    path_len: usize,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    let Some(path) = (unsafe { str_arg(path, path_len) }) else {
        return INVALID;
    };
    match abi.front.register_remove_relay(path) {
        Ok(()) => {
            clear_last_error(abi);
            OK
        }
        Err(error) => set_last_error(abi, error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_register_wstat_relay(
    handle: *mut FrontAbi,
    path: *const c_char,
    path_len: usize,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    let Some(path) = (unsafe { str_arg(path, path_len) }) else {
        return INVALID;
    };
    match abi.front.register_wstat_relay(path) {
        Ok(()) => {
            clear_last_error(abi);
            OK
        }
        Err(error) => set_last_error(abi, error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_register_log(
    handle: *mut FrontAbi,
    path: *const c_char,
    path_len: usize,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    let Some(path) = (unsafe { str_arg(path, path_len) }) else {
        return INVALID;
    };
    match abi.front.register_log(path) {
        Ok(()) => {
            clear_last_error(abi);
            OK
        }
        Err(error) => set_last_error(abi, error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_set_principal_root(
    handle: *mut FrontAbi,
    principal: *const c_char,
    principal_len: usize,
    root_path: *const c_char,
    root_path_len: usize,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    let (Some(principal), Some(root_path)) =
        (unsafe { str_arg(principal, principal_len) }, unsafe {
            str_arg(root_path, root_path_len)
        })
    else {
        return INVALID;
    };
    match abi.front.set_principal_root(principal, root_path) {
        Ok(()) => {
            clear_last_error(abi);
            OK
        }
        Err(error) => set_last_error(abi, error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_set_principal_root_aname(
    handle: *mut FrontAbi,
    principal: *const c_char,
    principal_len: usize,
    aname: *const c_char,
    aname_len: usize,
    root_path: *const c_char,
    root_path_len: usize,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    let (Some(principal), Some(aname), Some(root_path)) = (
        unsafe { str_arg(principal, principal_len) },
        unsafe { str_arg(aname, aname_len) },
        unsafe { str_arg(root_path, root_path_len) },
    ) else {
        return INVALID;
    };
    match abi
        .front
        .set_principal_root_aname(principal, aname, root_path)
    {
        Ok(()) => {
            clear_last_error(abi);
            OK
        }
        Err(error) => set_last_error(abi, error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_set_principal_class_aname(
    handle: *mut FrontAbi,
    uname: *const c_char,
    uname_len: usize,
    principal_id: *const c_char,
    principal_id_len: usize,
    aname: *const c_char,
    aname_len: usize,
    root_path: *const c_char,
    root_path_len: usize,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    let (Some(uname), Some(principal_id), Some(aname), Some(root_path)) = (
        unsafe { str_arg(uname, uname_len) },
        unsafe { str_arg(principal_id, principal_id_len) },
        unsafe { str_arg(aname, aname_len) },
        unsafe { str_arg(root_path, root_path_len) },
    ) else {
        return INVALID;
    };
    match abi
        .front
        .set_principal_class_aname(uname, principal_id, aname, root_path)
    {
        Ok(()) => {
            clear_last_error(abi);
            OK
        }
        Err(error) => set_last_error(abi, error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_set_protocol_limits(
    handle: *mut FrontAbi,
    max_msize: u32,
    iounit: u32,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    match abi.front.set_protocol_limits(max_msize, iounit) {
        Ok(()) => {
            clear_last_error(abi);
            OK
        }
        Err(error) => set_last_error(abi, error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_serve_tcp(
    handle: *mut FrontAbi,
    bind: *const c_char,
    bind_len: usize,
    port_out: *mut u16,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    let Some(bind) = (unsafe { str_arg(bind, bind_len) }) else {
        return INVALID;
    };
    match abi.front.serve_tcp(bind) {
        Ok(serve) => {
            if !port_out.is_null() {
                unsafe { *port_out = serve.addr().port() };
            }
            match abi.serves.lock() {
                Ok(mut serves) => {
                    serves.push(serve);
                    clear_last_error(abi);
                    OK
                }
                Err(error) => set_last_error(abi, error),
            }
        }
        Err(error) => set_last_error(abi, error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_stop(handle: *mut FrontAbi) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    match abi.serves.lock() {
        Ok(serves) => {
            for serve in serves.iter() {
                serve.shutdown();
            }
            clear_last_error(abi);
            OK
        }
        Err(error) => set_last_error(abi, error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_last_error(
    handle: *mut FrontAbi,
    buf: *mut u8,
    cap: usize,
) -> isize {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID as isize;
    };
    let Ok(last_error) = abi.last_error.lock() else {
        return INTERNAL as isize;
    };
    if cap > 0 {
        if buf.is_null() {
            return INVALID as isize;
        }
        let copied = cap.min(last_error.len());
        unsafe {
            std::ptr::copy_nonoverlapping(last_error.as_ptr(), buf, copied);
        }
    }
    last_error.len() as isize
}

#[cfg(test)]
mod tests {
    use super::str_arg;
    use std::{ffi::c_char, ptr};

    #[test]
    fn null_string_pointer_is_valid_only_for_an_empty_argument() {
        assert_eq!(unsafe { str_arg(ptr::null::<c_char>(), 0) }, Some(""));
        assert_eq!(unsafe { str_arg(ptr::null::<c_char>(), 1) }, None);
    }
}
