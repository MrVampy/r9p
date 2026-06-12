use front::abi::{
    r9p_front_abi_version, r9p_front_append_event, r9p_front_complete_request, r9p_front_free,
    r9p_front_new, r9p_front_next_request, r9p_front_register_intake, r9p_front_request_copy,
    r9p_front_serve_tcp, r9p_front_set, r9p_front_stop,
};
use r9p::blocking::Client;
use std::ffi::c_char;
use std::thread;
use std::time::{Duration, Instant};

fn cstr(value: &str) -> (*const c_char, usize) {
    (value.as_ptr().cast::<c_char>(), value.len())
}

fn cbytes(value: &[u8]) -> (*const u8, usize) {
    (value.as_ptr(), value.len())
}

#[test]
fn abi_roundtrip_over_tcp() {
    assert_eq!(r9p_front_abi_version(), 1);
    let handle = r9p_front_new();
    let (path, path_len) = cstr("market/status");
    let (bytes, bytes_len) = cbytes(b"#M(\"state\" 'open)");
    assert_eq!(
        unsafe { r9p_front_set(handle, path, path_len, bytes, bytes_len) },
        0
    );
    let (events_path, events_len) = cstr("market/events");
    let (seed, seed_len) = cbytes(b"seed\n");
    assert_eq!(
        unsafe { r9p_front_append_event(handle, events_path, events_len, seed, seed_len) },
        0
    );
    let (intake, intake_len) = cstr("queries");
    assert_eq!(
        unsafe { r9p_front_register_intake(handle, intake, intake_len) },
        0
    );
    let (bind, bind_len) = cstr("127.0.0.1:0");
    let mut port = 0u16;
    assert_eq!(
        unsafe { r9p_front_serve_tcp(handle, bind, bind_len, &mut port) },
        0
    );
    assert_ne!(port, 0);
    let address = format!("127.0.0.1:{port}");

    let mut client =
        Client::connect_tcp(&address, "claude", "/", 65536).expect("connect front");
    let status_fid = client.walk_path("/market/status").expect("walk status");
    client.open(status_fid, 0).expect("open status");
    let status = client.read(status_fid, 0, 4096).expect("read status");
    assert_eq!(status, b"#M(\"state\" 'open)".to_vec());

    let events_fid = client.walk_path("/market/events").expect("walk events");
    client.open(events_fid, 0).expect("open events");
    let seed_read = client.read(events_fid, 0, 4096).expect("read seed");
    assert_eq!(seed_read, b"seed\n".to_vec());

    let waker = unsafe { handle.as_ref() }.map(|_| ());
    assert!(waker.is_some());
    let wake_handle = handle as usize;
    let pusher = thread::spawn(move || {
        thread::sleep(Duration::from_millis(120));
        let revived = wake_handle as *mut front::abi::FrontAbi;
        let (path, path_len) = cstr("market/events");
        let (bytes, bytes_len) = cbytes(b"wake\n");
        unsafe { r9p_front_append_event(revived, path, path_len, bytes, bytes_len) }
    });
    let started = Instant::now();
    let woken = client.read(events_fid, 5, 4096).expect("blocking read");
    assert_eq!(woken, b"wake\n".to_vec());
    assert!(started.elapsed() >= Duration::from_millis(60));
    assert_eq!(pusher.join().expect("pusher join"), 0);

    let new_fid = client.walk_path("/queries/new").expect("walk new");
    client.open(new_fid, 1).expect("open new for write");
    let wrote = client
        .write_once(new_fid, 0, b"#M(\"kind\" \"search\" \"text\" \"Trump\")")
        .expect("write query");
    assert_eq!(wrote as usize, b"#M(\"kind\" \"search\" \"text\" \"Trump\")".len());

    let mut request_id = 0u64;
    let mut request_len = 0usize;
    assert_eq!(
        unsafe { r9p_front_next_request(handle, 1000, &mut request_id, &mut request_len) },
        0
    );
    assert_eq!(request_id, 1);
    let mut buf = vec![0u8; request_len];
    let copied = unsafe { r9p_front_request_copy(handle, buf.as_mut_ptr(), buf.len()) };
    assert_eq!(copied as usize, request_len);
    assert_eq!(buf, b"#M(\"kind\" \"search\" \"text\" \"Trump\")".to_vec());

    let (result, result_len) = cbytes(b"#M(\"hits\" (\"will-trump\" ))");
    assert_eq!(
        unsafe {
            r9p_front_complete_request(handle, intake, intake_len, request_id, result, result_len)
        },
        0
    );
    let result_fid = client.walk_path("/queries/1/result").expect("walk result");
    client.open(result_fid, 0).expect("open result");
    let result_read = client.read(result_fid, 0, 4096).expect("read result");
    assert_eq!(result_read, b"#M(\"hits\" (\"will-trump\" ))".to_vec());

    assert_eq!(unsafe { r9p_front_stop(handle) }, 0);
    unsafe { r9p_front_free(handle) };
}
