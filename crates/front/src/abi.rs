#![allow(clippy::missing_safety_doc)]

use crate::serve::ServeHandle;
use crate::{Front, IntakeRequest};
use std::ffi::c_char;
use std::sync::Mutex;
use std::time::Duration;

pub const ABI_VERSION: u32 = 1;

const OK: i32 = 0;
const TIMEOUT: i32 = 1;
const INVALID: i32 = -1;
const INTERNAL: i32 = -2;

pub struct FrontAbi {
    front: Front,
    serves: Mutex<Vec<ServeHandle>>,
    last_request: Mutex<Option<IntakeRequest>>,
}

unsafe fn str_arg<'a>(ptr: *const c_char, len: usize) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
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

#[no_mangle]
pub extern "C" fn r9p_front_abi_version() -> u32 {
    ABI_VERSION
}

#[no_mangle]
pub extern "C" fn r9p_front_new() -> *mut FrontAbi {
    Box::into_raw(Box::new(FrontAbi {
        front: Front::new(),
        serves: Mutex::new(Vec::new()),
        last_request: Mutex::new(None),
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
            serve.stop();
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
    let (Some(path), Some(bytes)) =
        (unsafe { str_arg(path, path_len) }, unsafe { bytes_arg(bytes, bytes_len) })
    else {
        return INVALID;
    };
    match abi.front.set(path, bytes) {
        Ok(()) => OK,
        Err(_) => INTERNAL,
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
    let (Some(path), Some(bytes)) =
        (unsafe { str_arg(path, path_len) }, unsafe { bytes_arg(bytes, bytes_len) })
    else {
        return INVALID;
    };
    match abi.front.append_event(path, bytes) {
        Ok(()) => OK,
        Err(_) => INTERNAL,
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
        Ok(()) => OK,
        Err(_) => INTERNAL,
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
                    OK
                }
                Err(_) => INTERNAL,
            }
        }
        Err(_) => INTERNAL,
    }
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
            unsafe {
                *id_out = request.request_id;
                *len_out = request.bytes.len();
            }
            match abi.last_request.lock() {
                Ok(mut slot) => {
                    *slot = Some(request);
                    OK
                }
                Err(_) => INTERNAL,
            }
        }
        Ok(None) => TIMEOUT,
        Err(_) => INTERNAL,
    }
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_request_copy(
    handle: *mut FrontAbi,
    buf: *mut u8,
    cap: usize,
) -> isize {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID as isize;
    };
    if buf.is_null() {
        return INVALID as isize;
    }
    let Ok(slot) = abi.last_request.lock() else {
        return INTERNAL as isize;
    };
    let Some(request) = slot.as_ref() else {
        return INVALID as isize;
    };
    if request.bytes.len() > cap {
        return INVALID as isize;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(request.bytes.as_ptr(), buf, request.bytes.len());
    }
    request.bytes.len() as isize
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
    let (Some(prefix), Some(bytes)) =
        (unsafe { str_arg(prefix, prefix_len) }, unsafe { bytes_arg(bytes, bytes_len) })
    else {
        return INVALID;
    };
    match abi.front.complete_request(prefix, request_id, bytes) {
        Ok(()) => OK,
        Err(_) => INTERNAL,
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
                serve.stop();
            }
            OK
        }
        Err(_) => INTERNAL,
    }
}
