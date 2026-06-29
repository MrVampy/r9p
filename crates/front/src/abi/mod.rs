#![allow(clippy::missing_safety_doc)]

use crate::serve::ServeHandle;
use crate::{Front, PushedDirectoryMetadata, PushedFileMetadata, RequestContext};
use r9p::srv_publish::R9pExportMaintainer;
use std::collections::BTreeMap;
use std::ffi::c_char;
use std::sync::Mutex;
use std::time::Duration;

mod client;
mod publication;

pub use client::{r9p_front_client_read, r9p_front_client_rpc};
pub use publication::{
    r9p_front_maintain_r9p_export, r9p_front_publish_r9p_export, r9p_front_reconcile_r9p_exports,
};

pub const ABI_VERSION: u32 = 14;

const OK: i32 = 0;
const TIMEOUT: i32 = 1;
const INVALID: i32 = -1;
const INTERNAL: i32 = -2;

pub struct FrontAbi {
    front: Front,
    serves: Mutex<Vec<ServeHandle>>,
    publications: Mutex<Vec<R9pExportMaintainer>>,
    staged_requests: Mutex<BTreeMap<u64, StagedRequest>>,
    last_error: Mutex<Vec<u8>>,
}

struct StagedRequest {
    prefix: Vec<u8>,
    bytes: Vec<u8>,
    context: Vec<u8>,
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

fn request_context_lfe(context: &RequestContext) -> Vec<u8> {
    format!(
        "#M(\"version\" \"r9p-front-request-context.v1\" \"principal_id\" \"{}\" \"uname\" \"{}\" \"aname\" \"{}\" \"session_id\" {} \"fid\" {} \"target_path\" \"{}\" \"offset\" {} \"open_mode\" {} \"pushed_generation\" {})",
        escape_lfe_string(&context.principal_id),
        escape_lfe_string(&context.uname),
        escape_lfe_string(&context.aname),
        context.session_id,
        context.fid,
        escape_lfe_string(&context.target_path),
        context.offset,
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
pub extern "C" fn r9p_front_abi_version() -> u32 {
    ABI_VERSION
}

#[no_mangle]
pub extern "C" fn r9p_front_new() -> *mut FrontAbi {
    Box::into_raw(Box::new(FrontAbi {
        front: Front::new(),
        serves: Mutex::new(Vec::new()),
        publications: Mutex::new(Vec::new()),
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
    if let Ok(mut publications) = abi.publications.lock() {
        for publication in publications.drain(..) {
            publication.shutdown();
        }
        drop(publications);
    }
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
    if buf.is_null() {
        return INVALID as isize;
    }
    let Ok(mut requests) = abi.staged_requests.lock() else {
        return INTERNAL as isize;
    };
    let Some(request) = requests.get(&request_id) else {
        return INVALID as isize;
    };
    if request.bytes.len() > cap {
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
pub unsafe extern "C" fn r9p_front_stop(handle: *mut FrontAbi) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    if let Err(error) = publication::stop_publications(abi) {
        return set_last_error(abi, error);
    }
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
