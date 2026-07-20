use front::abi::{
    r9p_front_client_create_at, r9p_front_client_create_write_at, r9p_front_client_remove,
    r9p_front_client_write_file, r9p_front_free, r9p_front_last_error, r9p_front_new, FrontAbi,
};
use fs::{LocalTree, LocalTreeConfig};
use r9p::{codec, server::Server};
use std::{
    ffi::c_char,
    fs as std_fs,
    net::{TcpListener, TcpStream},
    path::PathBuf,
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

type TestResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn cstr(value: &str) -> (*const c_char, usize) {
    (value.as_ptr().cast::<c_char>(), value.len())
}

#[test]
fn client_mutations_use_native_9p_operations() -> TestResult<()> {
    let root = fixture_root("client-mutations")?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?.to_string();
    let server_root = root.clone();
    let server = thread::spawn(move || -> TestResult<()> {
        for _ in 0..4 {
            let (stream, _) = listener.accept()?;
            let tree =
                LocalTree::open_with_config(&server_root, LocalTreeConfig { writable: true })?;
            serve_connection(stream, tree)?;
        }
        Ok(())
    });

    let handle = r9p_front_new();
    let target = TargetArgs::new(&address);
    let (parent, parent_len) = cstr("/");
    let (name, name_len) = cstr("created");
    let mut qid_type = 0_u8;
    let mut _qid_version = 0_u32;
    let mut qid_path = 0_u64;
    let create_status = unsafe {
        r9p_front_client_create_at(
            handle,
            target.endpoint,
            target.endpoint_len,
            target.uname,
            target.uname_len,
            target.aname,
            target.aname_len,
            parent,
            parent_len,
            name,
            name_len,
            0o600,
            1,
            65_536,
            &mut qid_type,
            &mut _qid_version,
            &mut qid_path,
        )
    };
    assert_status(handle, create_status);
    assert_eq!(qid_type, 0);
    assert_ne!(qid_path, 0);

    let (path, path_len) = cstr("/created");
    let body = b"native write\n";
    let mut count = 0_u32;
    let write_status = unsafe {
        r9p_front_client_write_file(
            handle,
            target.endpoint,
            target.endpoint_len,
            target.uname,
            target.uname_len,
            target.aname,
            target.aname_len,
            path,
            path_len,
            body.as_ptr(),
            body.len(),
            65_536,
            &mut count,
        )
    };
    assert_status(handle, write_status);
    assert_eq!(count as usize, body.len());
    assert_eq!(std_fs::read(root.join("created"))?, body);

    let (atomic_name, atomic_name_len) = cstr("atomic");
    let atomic_body = b"create and write\n";
    let mut atomic_count = 0_u32;
    let atomic_status = unsafe {
        r9p_front_client_create_write_at(
            handle,
            target.endpoint,
            target.endpoint_len,
            target.uname,
            target.uname_len,
            target.aname,
            target.aname_len,
            parent,
            parent_len,
            atomic_name,
            atomic_name_len,
            0o600,
            1,
            0,
            atomic_body.as_ptr(),
            atomic_body.len(),
            65_536,
            &mut atomic_count,
        )
    };
    assert_status(handle, atomic_status);
    assert_eq!(atomic_count as usize, atomic_body.len());
    assert_eq!(std_fs::read(root.join("atomic"))?, atomic_body);

    let remove_status = unsafe {
        r9p_front_client_remove(
            handle,
            target.endpoint,
            target.endpoint_len,
            target.uname,
            target.uname_len,
            target.aname,
            target.aname_len,
            path,
            path_len,
            65_536,
        )
    };
    assert_status(handle, remove_status);
    assert!(!root.join("created").exists());

    unsafe { r9p_front_free(handle) };
    server
        .join()
        .map_err(|_| "mutation server thread panicked")??;
    std_fs::remove_dir_all(root)?;
    Ok(())
}

fn assert_status(handle: *mut FrontAbi, status: i32) {
    if status == 0 {
        return;
    }
    let error_len = unsafe { r9p_front_last_error(handle, std::ptr::null_mut(), 0) };
    let mut error = vec![0; usize::try_from(error_len).unwrap_or_default()];
    if !error.is_empty() {
        unsafe {
            r9p_front_last_error(handle, error.as_mut_ptr(), error.len());
        }
    }
    panic!(
        "front ABI status {status}: {}",
        String::from_utf8_lossy(&error)
    );
}

struct TargetArgs {
    endpoint: *const c_char,
    endpoint_len: usize,
    uname: *const c_char,
    uname_len: usize,
    aname: *const c_char,
    aname_len: usize,
}

impl TargetArgs {
    fn new(endpoint: &str) -> Self {
        let (endpoint, endpoint_len) = cstr(endpoint);
        let (uname, uname_len) = cstr("codex");
        let (aname, aname_len) = cstr("/");
        Self {
            endpoint,
            endpoint_len,
            uname,
            uname_len,
            aname,
            aname_len,
        }
    }
}

fn serve_connection(mut stream: TcpStream, tree: LocalTree) -> TestResult<()> {
    let mut server = Server::new(tree);
    while let Some(message) = codec::read_tmessage_checked(&mut stream, server.session().msize())? {
        let reply = server.handle(message);
        codec::write_rmessage_checked(&mut stream, server.session().msize(), &reply)?;
    }
    Ok(())
}

fn fixture_root(label: &str) -> TestResult<PathBuf> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let path =
        std::env::temp_dir().join(format!("r9p-front-{label}-{}-{nanos}", std::process::id()));
    std_fs::create_dir(&path)?;
    Ok(path)
}
