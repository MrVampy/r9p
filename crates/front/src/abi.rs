#![allow(clippy::missing_safety_doc)]

use crate::serve::ServeHandle;
use crate::Front;
use r9p::export_descriptor::{
    AuthBoundary, ExportDescriptor, ExportMode, Protocol, TransportClass,
};
use r9p::srv_publish::{publish_r9p_export, R9pExportPublication};
use std::collections::BTreeMap;
use std::ffi::c_char;
use std::sync::Mutex;
use std::time::Duration;

pub const ABI_VERSION: u32 = 5;

const OK: i32 = 0;
const TIMEOUT: i32 = 1;
const INVALID: i32 = -1;
const INTERNAL: i32 = -2;

pub struct FrontAbi {
    front: Front,
    serves: Mutex<Vec<ServeHandle>>,
    staged_requests: Mutex<BTreeMap<u64, Vec<u8>>>,
    last_error: Mutex<Vec<u8>>,
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

unsafe fn optional_str_arg<'a>(ptr: *const c_char, len: usize) -> Option<Option<&'a str>> {
    if len == 0 {
        return Some(None);
    }
    unsafe { str_arg(ptr, len) }.map(Some)
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
                    requests.insert(request_id, request.bytes);
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
    if buf.is_null() {
        return INVALID as isize;
    }
    let Ok(mut requests) = abi.staged_requests.lock() else {
        return INTERNAL as isize;
    };
    let Some(request) = requests.get(&request_id) else {
        return INVALID as isize;
    };
    if request.len() > cap {
        return INVALID as isize;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(request.as_ptr(), buf, request.len());
    }
    let len = request.len();
    requests.remove(&request_id);
    len as isize
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
pub unsafe extern "C" fn r9p_front_publish_r9p_export(
    handle: *mut FrontAbi,
    vault_endpoint_bind: *const c_char,
    vault_endpoint_bind_len: usize,
    vault_uname: *const c_char,
    vault_uname_len: usize,
    vault_aname: *const c_char,
    vault_aname_len: usize,
    service_name: *const c_char,
    service_name_len: usize,
    export_endpoint_bind: *const c_char,
    export_endpoint_bind_len: usize,
    export_uname: *const c_char,
    export_uname_len: usize,
    export_aname: *const c_char,
    export_aname_len: usize,
    exported_root: *const c_char,
    exported_root_len: usize,
    transport_class: *const c_char,
    transport_class_len: usize,
    auth: *const c_char,
    auth_len: usize,
    protocol: *const c_char,
    protocol_len: usize,
    local_root_label: *const c_char,
    local_root_label_len: usize,
    pid: u32,
    msize: u32,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    let (
        Some(vault_endpoint_bind),
        Some(vault_uname),
        Some(vault_aname),
        Some(service_name),
        Some(export_endpoint_bind),
        Some(export_uname),
        Some(export_aname),
        Some(exported_root),
        Some(transport_class),
        Some(auth),
        Some(protocol),
        Some(local_root_label),
    ) = (
        unsafe { str_arg(vault_endpoint_bind, vault_endpoint_bind_len) },
        unsafe { str_arg(vault_uname, vault_uname_len) },
        unsafe { str_arg(vault_aname, vault_aname_len) },
        unsafe { str_arg(service_name, service_name_len) },
        unsafe { str_arg(export_endpoint_bind, export_endpoint_bind_len) },
        unsafe { str_arg(export_uname, export_uname_len) },
        unsafe { str_arg(export_aname, export_aname_len) },
        unsafe { str_arg(exported_root, exported_root_len) },
        unsafe { str_arg(transport_class, transport_class_len) },
        unsafe { str_arg(auth, auth_len) },
        unsafe { str_arg(protocol, protocol_len) },
        unsafe { optional_str_arg(local_root_label, local_root_label_len) },
    )
    else {
        return INVALID;
    };
    let transport_class = match TransportClass::parse(transport_class) {
        Ok(value) => value,
        Err(error) => return set_last_error(abi, error),
    };
    let auth = match AuthBoundary::parse(auth) {
        Ok(value) => value,
        Err(error) => return set_last_error(abi, error),
    };
    let protocol = match Protocol::parse(protocol) {
        Ok(value) => value,
        Err(error) => return set_last_error(abi, error),
    };
    let descriptor = ExportDescriptor {
        endpoint_bind: export_endpoint_bind.to_string(),
        aname: export_aname.to_string(),
        uname: export_uname.to_string(),
        exported_root: exported_root.to_string(),
        transport_class,
        mode: ExportMode::ReadOnly,
        auth,
        pid,
        protocol,
        msize,
        expires_at: None,
        local_root_label: local_root_label.map(str::to_string),
        extra_fields: BTreeMap::new(),
    };
    let publication = R9pExportPublication {
        vault_endpoint_bind: vault_endpoint_bind.to_string(),
        vault_uname: vault_uname.to_string(),
        vault_aname: vault_aname.to_string(),
        service_name: service_name.to_string(),
        descriptor,
    };
    match publish_r9p_export(&publication) {
        Ok(_) => {
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
