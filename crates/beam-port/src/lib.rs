mod front_port;
mod hex;
mod stdio;

use r9p::{
    blocking::{self, ReadWrite},
    client::{Client as ProtocolClient, ClientResponse, Completion},
    codec,
    error::{Error as R9pError, Result as R9pResult},
    qid::Qid,
    stat::Stat,
};
use session::{
    AuthorityBindings, Client as NamespaceClient, ConnectionConfig, Error as SessionError,
    Result as SessionResult,
};
use std::{collections::HashMap, path::PathBuf, time::Duration};

#[cfg(unix)]
use std::{os::unix::net::UnixStream, path::Path};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct TargetKey {
    bind: String,
    uname: String,
    aname: String,
    msize: u32,
    auth_config: Option<PathBuf>,
    authorities: AuthorityBindings,
}

const AUTH_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Default)]
struct PeerClientServer {
    clients: HashMap<TargetKey, NamespaceClient>,
    fronts: front_port::FrontManager,
}

pub fn run_stdio() -> Result<(), String> {
    stdio::run()
}

impl PeerClientServer {
    #[cfg(test)]
    fn handle_line(&mut self, line: &str) -> Result<String, String> {
        self.dispatch_line(line).complete()
    }

    fn dispatch_line(&mut self, line: &str) -> stdio::ResponseWork {
        let fields = line
            .trim_end_matches(['\r', '\n'])
            .split('\t')
            .collect::<Vec<_>>();
        if fields
            .first()
            .is_some_and(|operation| operation.starts_with("front-"))
        {
            return match self.fronts.pending_request(&fields) {
                Some(Ok(request)) => stdio::ResponseWork::Pending(request),
                Some(Err(error)) => stdio::ResponseWork::Ready(Err(error)),
                None => stdio::ResponseWork::Ready(self.fronts.handle(&fields)),
            };
        }
        let Some((operation, fields)) = fields.split_first() else {
            return stdio::ResponseWork::Ready(Err("invalid_r9p_beam_port_request".to_string()));
        };
        let response = target_and_args(fields).and_then(|(key, args)| match (*operation, args) {
            ("version", []) => version_probe_output(&key).map_err(|error| error.to_string()),
            ("attach", []) => self.with_client_retry(&key, attach_output),
            ("stat", [path]) => {
                let path = hex::decode_text(path)?;
                self.with_client_retry(&key, |client| stat_output(client, &path))
            }
            ("list", [path]) => {
                let path = hex::decode_text(path)?;
                self.with_client_retry(&key, |client| list_output(client, &path))
            }
            ("read", [path]) => {
                let path = hex::decode_text(path)?;
                self.with_client_retry(&key, |client| read_output(client, &path))
            }
            ("read-range", [path, offset, count]) => {
                let path = hex::decode_text(path)?;
                let offset = parse_u64("offset", offset)?;
                let count = parse_u32("count", count)?;
                self.with_client_retry(&key, |client| {
                    read_range_output(client, &path, offset, count)
                })
            }
            ("write", [path, offset, data]) => {
                let path = hex::decode_text(path)?;
                let offset = parse_u64("offset", offset)?;
                let data = hex::decode(data)?;
                self.with_client(&key, |client| write_output(client, &path, offset, &data))
            }
            ("write-file", [path, data]) => {
                let path = hex::decode_text(path)?;
                let data = hex::decode(data)?;
                self.with_client(&key, |client| write_file_output(client, &path, &data))
            }
            ("rpc", [path, data]) => {
                let path = hex::decode_text(path)?;
                let data = hex::decode(data)?;
                self.with_client(&key, |client| rpc_output(client, &path, &data))
            }
            ("create", [path, perm, mode]) => {
                let path = hex::decode_text(path)?;
                let perm = parse_u32("perm", perm)?;
                let mode = parse_u8("mode", mode)?;
                self.with_client(&key, |client| create_output(client, &path, perm, mode))
            }
            ("create-at", [parent, name, perm, mode]) => {
                let parent = hex::decode_text(parent)?;
                let name = hex::decode_text(name)?;
                let perm = parse_u32("perm", perm)?;
                let mode = parse_u8("mode", mode)?;
                self.with_client(&key, |client| {
                    create_at_output(client, &parent, &name, perm, mode)
                })
            }
            ("create-write-at", [parent, name, perm, mode, offset, data]) => {
                let parent = hex::decode_text(parent)?;
                let name = hex::decode_text(name)?;
                let perm = parse_u32("perm", perm)?;
                let mode = parse_u8("mode", mode)?;
                let offset = parse_u64("offset", offset)?;
                let data = hex::decode(data)?;
                self.with_client(&key, |client| {
                    create_write_at_output(client, &parent, &name, perm, mode, offset, &data)
                })
            }
            ("remove", [path]) => {
                let path = hex::decode_text(path)?;
                self.with_client(&key, |client| remove_output(client, &path))
            }
            _ => Err("invalid_r9p_beam_port_request".to_string()),
        });
        stdio::ResponseWork::Ready(response)
    }

    fn with_client_retry(
        &mut self,
        key: &TargetKey,
        operation: impl Fn(&NamespaceClient) -> SessionResult<String> + Copy,
    ) -> Result<String, String> {
        match self.with_client(key, operation) {
            Ok(output) => Ok(output),
            Err(reason) if retryable_client_error(&reason) => {
                let _ = self.clients.remove(key);
                self.with_client(key, operation)
                    .map_err(|second| format!("{reason}; retry: {second}"))
            }
            Err(reason) => Err(reason),
        }
    }

    fn with_client(
        &mut self,
        key: &TargetKey,
        operation: impl FnOnce(&NamespaceClient) -> SessionResult<String>,
    ) -> Result<String, String> {
        if !self.clients.contains_key(key) {
            let client = connect_client(key).map_err(|error| error.to_string())?;
            self.clients.insert(key.clone(), client);
        }

        let result = {
            let client = self
                .clients
                .get(key)
                .ok_or_else(|| "r9p_beam_port_missing_cached_client".to_string())?;
            operation(client).map_err(|error| error.to_string())
        };

        if result.is_err() {
            let _ = self.clients.remove(key);
        }

        result
    }
}

fn connect_client(key: &TargetKey) -> SessionResult<NamespaceClient> {
    NamespaceClient::connect_with_timeout(
        &ConnectionConfig {
            auth_domain: None,
            address: key.bind.clone(),
            uname: key.uname.clone(),
            aname: key.aname.clone(),
            msize: key.msize,
            auth_config: key.auth_config.clone(),
            authorities: key.authorities.clone(),
        },
        AUTH_HANDSHAKE_TIMEOUT,
    )
}

fn connect_stream(key: &TargetKey) -> R9pResult<Box<dyn ReadWrite>> {
    #[cfg(unix)]
    if let Some(path) = key
        .bind
        .strip_prefix("unix!")
        .or_else(|| key.bind.strip_prefix("unix:"))
    {
        if key.auth_config.is_some() {
            return Err(R9pError::from(
                "session auth config is only valid for TCP endpoints",
            ));
        }
        let stream = UnixStream::connect(Path::new(path))
            .map_err(|error| R9pError::from(format!("connect {path}: {error}")))?;
        return Ok(Box::new(stream));
    }

    let stream = blocking::connect_tcp_stream(&key.bind)?;
    match &key.auth_config {
        Some(path) => {
            let auth = r9p_auth::ClientConfig::read(path)?;
            r9p_auth::authenticate_client(stream, &auth, &key.uname, AUTH_HANDSHAKE_TIMEOUT)
                .map(|stream| Box::new(stream) as Box<dyn ReadWrite>)
        }
        None => Ok(Box::new(stream)),
    }
}

fn version_probe_output(key: &TargetKey) -> R9pResult<String> {
    let mut stream = connect_stream(key)?;
    let mut protocol = ProtocolClient::new();
    let request = protocol.version_request(key.msize);
    codec::write_tmessage_checked(&mut stream, key.msize, &request)?;
    let response = codec::read_rmessage_checked(&mut stream, key.msize)?
        .ok_or_else(|| R9pError::from("9P transport closed before version response"))?;

    match protocol.receive(response)? {
        ClientResponse::Completion {
            completion: Completion::Version { msize, version },
            ..
        } => Ok(format!("version\t{}\t{msize}", hex::encode(&version))),
        ClientResponse::Error { ename, .. } => Err(R9pError::new(ename)),
        other => Err(R9pError::from(format!(
            "unexpected version response: {other:?}"
        ))),
    }
}

fn attach_output(client: &NamespaceClient) -> SessionResult<String> {
    client
        .stat(client.root_fid())
        .map(|stat| format_qid("attach", stat.qid))
}

fn stat_output(client: &NamespaceClient, path: &str) -> SessionResult<String> {
    client
        .stat_path(path)
        .map(|stat| format_stat("stat", &stat))
}

fn list_output(client: &NamespaceClient, path: &str) -> SessionResult<String> {
    client.list_path(path).map(|stats| format_stat_list(&stats))
}

fn format_stat_list(stats: &[Stat]) -> String {
    stats
        .iter()
        .map(|stat| format_stat("entry", stat))
        .collect::<Vec<_>>()
        .join("\n")
}

fn read_output(client: &NamespaceClient, path: &str) -> SessionResult<String> {
    client
        .read_path(path)
        .map(|bytes| format!("read\t{}", hex::encode(&bytes)))
}

fn read_range_output(
    client: &NamespaceClient,
    path: &str,
    offset: u64,
    count: u32,
) -> SessionResult<String> {
    client
        .read_path_range(path, offset, count)
        .map(|bytes| format!("read\t{}", hex::encode(&bytes)))
}

fn write_output(
    client: &NamespaceClient,
    path: &str,
    offset: u64,
    data: &[u8],
) -> SessionResult<String> {
    client
        .write_path(path, offset, data)
        .map(|count| format!("write\t{count}"))
}

fn write_file_output(client: &NamespaceClient, path: &str, data: &[u8]) -> SessionResult<String> {
    client
        .write_file(path, data)
        .map(|count| format!("write-file\t{count}"))
}

fn rpc_output(client: &NamespaceClient, path: &str, data: &[u8]) -> SessionResult<String> {
    client
        .rpc_path(path, data)
        .map(|bytes| format!("rpc\t{}\t{}", bytes.len(), hex::encode(&bytes)))
}

fn create_output(
    client: &NamespaceClient,
    path: &str,
    perm: u32,
    mode: u8,
) -> SessionResult<String> {
    let (parent, name) =
        split_parent(path).map_err(|error| SessionError::new(libc::EINVAL, error))?;
    create_at_output(client, &parent, &name, perm, mode)
}

fn create_at_output(
    client: &NamespaceClient,
    parent: &str,
    name: &str,
    perm: u32,
    mode: u8,
) -> SessionResult<String> {
    let qid = client.create_at(parent, name, perm, mode)?;
    let iounit = r9p::codec::max_write_payload(client.msize());
    Ok(format!(
        "create\t{}\t{}\t{}\t{}",
        qid.qtype, qid.version, qid.path, iounit
    ))
}

fn create_write_at_output(
    client: &NamespaceClient,
    parent: &str,
    name: &str,
    perm: u32,
    mode: u8,
    offset: u64,
    data: &[u8],
) -> SessionResult<String> {
    client
        .create_write_at(parent, name, perm, mode, offset, data)
        .map(|count| format!("create-write-at\t{count}"))
}

fn remove_output(client: &NamespaceClient, path: &str) -> SessionResult<String> {
    client.remove_path(path)?;
    Ok("remove".to_string())
}

fn target_and_args<'a>(fields: &'a [&'a str]) -> Result<(TargetKey, &'a [&'a str]), String> {
    let [bind, uname, aname, msize, auth_config, binding_count, rest @ ..] = fields else {
        return Err("invalid_r9p_beam_port_target".to_string());
    };
    let binding_count = binding_count
        .parse::<usize>()
        .map_err(|_| format!("invalid_authority_binding_count:{binding_count}"))?;
    let binding_fields = binding_count
        .checked_mul(2)
        .ok_or_else(|| "authority_binding_count_overflow".to_string())?;
    if rest.len() < binding_fields {
        return Err("incomplete_r9p_beam_port_authority_bindings".to_string());
    }
    let (raw_bindings, args) = rest.split_at(binding_fields);
    let mut authorities = AuthorityBindings::new();
    for pair in raw_bindings.chunks_exact(2) {
        let boundary = hex::decode_text(pair[0])?;
        let config_path = PathBuf::from(hex::decode_text(pair[1])?);
        authorities
            .insert_session_auth(boundary, config_path)
            .map_err(|error| error.to_string())?;
    }
    target_key(bind, uname, aname, msize, auth_config)
        .map(|key| (TargetKey { authorities, ..key }, args))
}

fn target_key(
    bind: &str,
    uname: &str,
    aname: &str,
    msize: &str,
    auth_config: &str,
) -> Result<TargetKey, String> {
    let auth_config = hex::decode_text(auth_config)?;
    Ok(TargetKey {
        bind: hex::decode_text(bind)?,
        uname: hex::decode_text(uname)?,
        aname: hex::decode_text(aname)?,
        msize: parse_u32("msize", msize)?,
        auth_config: if auth_config.is_empty() {
            None
        } else {
            Some(PathBuf::from(auth_config))
        },
        authorities: AuthorityBindings::new(),
    })
}

fn format_stat(prefix: &str, stat: &Stat) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        prefix,
        hex::encode(&stat.name),
        stat.qid.qtype,
        stat.qid.version,
        stat.qid.path,
        stat.length,
        stat.mode,
        stat.atime,
        stat.mtime,
        stat.type_,
        stat.dev,
        hex::encode(&stat.uid),
        hex::encode(&stat.gid),
        hex::encode(&stat.muid),
    )
}

fn format_qid(prefix: &str, qid: Qid) -> String {
    format!("{}\t{}\t{}\t{}", prefix, qid.qtype, qid.version, qid.path)
}

pub(crate) fn parse_u64(field: &str, value: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map_err(|_| format!("invalid_{field}:{value}"))
}

pub(crate) fn parse_u32(field: &str, value: &str) -> Result<u32, String> {
    value
        .parse::<u32>()
        .map_err(|_| format!("invalid_{field}:{value}"))
}

pub(crate) fn parse_u8(field: &str, value: &str) -> Result<u8, String> {
    value
        .parse::<u8>()
        .map_err(|_| format!("invalid_{field}:{value}"))
}

fn split_parent(path: &str) -> Result<(String, String), String> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("cannot_create_root".to_string());
    }
    let (parent, name) = match trimmed.rsplit_once('/') {
        Some(("", name)) => ("/".to_string(), name.to_string()),
        Some((parent, name)) => (parent.to_string(), name.to_string()),
        None => (".".to_string(), trimmed.to_string()),
    };
    if name.is_empty() || name == "." || name == ".." {
        return Err(format!("bad_create_name:{name}"));
    }
    Ok((parent, name))
}

fn retryable_client_error(reason: &str) -> bool {
    let normalized = reason.to_ascii_lowercase();
    !(normalized.contains("no such file")
        || normalized.contains("file does not exist")
        || normalized.contains("not_found")
        || normalized.contains("partial walk"))
}

fn response_line(response: Result<String, String>) -> String {
    match response {
        Ok(payload) => format!("ok\t{}", hex::encode(payload.as_bytes())),
        Err(reason) => format!("error\t{}", hex::encode(reason.as_bytes())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use r9p_auth::{generate_key_pair, write_key_pair};
    use std::{
        env, fs,
        os::unix::net::UnixListener,
        thread,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn response_line_hex_encodes_payload() {
        assert_eq!(response_line(Ok("read\t".to_string())), "ok\t7265616409");
    }

    #[test]
    fn hex_roundtrip_decodes_text() {
        assert_eq!(
            hex::decode_text("2f76656e756573"),
            Ok("/venues".to_string())
        );
    }

    #[test]
    fn target_key_decodes_text_and_msize() {
        let parsed = target_key(
            "746370213132372e302e302e312139353634",
            "636f646578",
            "2f",
            "65536",
            "",
        );
        assert_eq!(
            parsed,
            Ok(TargetKey {
                bind: "tcp!127.0.0.1!9564".to_string(),
                uname: "codex".to_string(),
                aname: "/".to_string(),
                msize: 65_536,
                auth_config: None,
                authorities: AuthorityBindings::new(),
            }),
        );
    }

    #[test]
    fn target_key_keeps_session_auth_in_the_client_cache_identity() {
        let parsed = target_key(
            "746370213139322e302e322e312139353634",
            "636f646578",
            "2f",
            "65536",
            "2f6574632f7239702f636c69656e742e636f6e66",
        );
        assert_eq!(
            parsed,
            Ok(TargetKey {
                bind: "tcp!192.0.2.1!9564".to_string(),
                uname: "codex".to_string(),
                aname: "/".to_string(),
                msize: 65_536,
                auth_config: Some(PathBuf::from("/etc/r9p/client.conf")),
                authorities: AuthorityBindings::new(),
            }),
        );
    }

    #[test]
    fn unix_targets_reject_session_auth_before_connecting() {
        let key = TargetKey {
            bind: "unix:/tmp/r9p-unused.sock".to_string(),
            uname: "codex".to_string(),
            aname: "/".to_string(),
            msize: 65_536,
            auth_config: Some(PathBuf::from("/etc/r9p/client.conf")),
            authorities: AuthorityBindings::new(),
        };

        let error = match connect_stream(&key) {
            Ok(_) => panic!("unix auth config must be rejected"),
            Err(error) => error,
        };
        assert_eq!(
            error.to_string(),
            "session auth config is only valid for TCP endpoints"
        );
    }

    #[test]
    fn split_parent_rejects_invalid_paths() {
        assert_eq!(
            split_parent("/srv/example"),
            Ok(("/srv".to_string(), "example".to_string()))
        );
        assert_eq!(split_parent("/"), Err("cannot_create_root".to_string()));
        assert_eq!(
            split_parent("/srv/.."),
            Err("bad_create_name:..".to_string())
        );
    }

    #[test]
    fn connect_stream_accepts_descriptor_unix_bind() {
        let socket_path = temp_socket_path("colon-bind");
        let _ = fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path);
        assert!(listener.is_ok());
        let listener = match listener {
            Ok(listener) => listener,
            Err(error) => panic!("unix listener should bind: {error}"),
        };
        let handle = thread::spawn(move || {
            let accepted = listener.accept();
            assert!(accepted.is_ok());
        });

        let key = TargetKey {
            bind: format!("unix:{}", socket_path.display()),
            uname: "codex".to_string(),
            aname: "/".to_string(),
            msize: 65_536,
            auth_config: None,
            authorities: AuthorityBindings::new(),
        };
        let stream = connect_stream(&key);
        assert!(stream.is_ok());
        let joined = handle.join();
        assert!(joined.is_ok());
        let _ = fs::remove_file(socket_path);
    }

    #[test]
    fn client_commands_report_version_and_attach() {
        let mut server = PeerClientServer::default();
        let front_id = parse_front_id(&server.handle_line("front-new").expect("front-new"));
        let serve = format!("front-serve-tcp\t{front_id}\t{}", hex_text("127.0.0.1:0"));
        let addr = parse_front_addr(&server.handle_line(&serve).expect("front serve"));
        let target = target_fields(&addr);

        let version = server
            .handle_line(&format!("version\t{target}"))
            .expect("version");
        let version_fields = version.split('\t').collect::<Vec<_>>();
        assert_eq!(version_fields[0], "version");
        assert_eq!(
            hex::decode_text(version_fields[1]).expect("version text"),
            "9P2000"
        );
        assert_eq!(version_fields[2], "65536");

        let attach = server
            .handle_line(&format!("attach\t{target}"))
            .expect("attach");
        let attach_fields = attach.split('\t').collect::<Vec<_>>();
        assert_eq!(attach_fields[0], "attach");
        assert_eq!(attach_fields.len(), 4);

        let stop = format!("front-stop\t{front_id}");
        assert_eq!(
            server.handle_line(&stop).expect("front-stop"),
            "front-stop".to_string()
        );
    }

    #[test]
    fn front_commands_serve_static_file() {
        let mut server = PeerClientServer::default();
        let front_id = parse_front_id(&server.handle_line("front-new").expect("front-new"));
        assert_eq!(
            server
                .handle_line(&format!("front-process-id\t{front_id}"))
                .expect("front process id"),
            format!("front-process-id\t{}", std::process::id())
        );
        let set = format!(
            "front-set\t{front_id}\t{}\t{}",
            hex_text("status"),
            hex::encode(b"running")
        );
        assert_eq!(
            server.handle_line(&set).expect("front-set"),
            "front-set".to_string()
        );
        let serve = format!("front-serve-tcp\t{front_id}\t{}", hex_text("127.0.0.1:0"));
        let addr = parse_front_addr(&server.handle_line(&serve).expect("front serve"));

        let mut client =
            blocking::Client::connect_tcp(&addr, "codex", "/", 65_536).expect("connect front");
        let body = client.read_path("status").expect("read status");
        assert_eq!(body, b"running");

        let stop = format!("front-stop\t{front_id}");
        assert_eq!(
            server.handle_line(&stop).expect("front-stop"),
            "front-stop".to_string()
        );
    }

    #[test]
    fn front_commands_serve_authenticated_static_file() {
        let root = temp_directory_path("authenticated-front");
        fs::create_dir(&root).expect("auth test directory");
        let server_key = generate_key_pair().expect("server key pair");
        let client_key = generate_key_pair().expect("client key pair");
        let server_private = root.join("server.key");
        let server_public = root.join("server.pub");
        let client_private = root.join("client.key");
        let client_public = root.join("client.pub");
        write_key_pair(&server_private, &server_public, &server_key)
            .expect("write server key pair");
        write_key_pair(&client_private, &client_public, &client_key)
            .expect("write client key pair");
        let server_config = root.join("server.conf");
        let client_config = root.join("client.conf");
        fs::write(
            &server_config,
            format!(
                "format r9p-session-auth.v1\nrole server\ndomain front-test\nprivate-key {}\npeer {} codex\n",
                server_private.display(),
                client_key.public
            ),
        )
        .expect("write server config");
        fs::write(
            &client_config,
            format!(
                "format r9p-session-auth.v1\nrole client\ndomain front-test\nprivate-key {}\nserver-key {}\n",
                client_private.display(),
                server_key.public
            ),
        )
        .expect("write client config");

        let mut server = PeerClientServer::default();
        let front_id = parse_front_id(&server.handle_line("front-new").expect("front-new"));
        let set = format!(
            "front-set\t{front_id}\t{}\t{}",
            hex_text("status"),
            hex::encode(b"authenticated")
        );
        assert_eq!(
            server.handle_line(&set).expect("front-set"),
            "front-set".to_string()
        );
        let serve = format!(
            "front-serve-tcp-authenticated\t{front_id}\t{}\t{}",
            hex_text("127.0.0.1:0"),
            hex_text(&server_config.to_string_lossy())
        );
        let addr = parse_authenticated_front_addr(
            &server
                .handle_line(&serve)
                .expect("authenticated front serve"),
        );
        let target = target_fields_with_auth(&addr, &client_config.to_string_lossy());
        let read = server
            .handle_line(&format!("read\t{target}\t{}", hex_text("status")))
            .expect("authenticated read");
        let fields = read.split('\t').collect::<Vec<_>>();
        assert_eq!(fields[0], "read");
        assert_eq!(
            hex::decode(&fields[1]).expect("read bytes"),
            b"authenticated"
        );

        let stop = format!("front-stop\t{front_id}");
        assert_eq!(
            server.handle_line(&stop).expect("front-stop"),
            "front-stop".to_string()
        );
        fs::remove_dir_all(root).expect("remove auth test directory");
    }

    #[test]
    fn front_commands_remove_projected_subtree() {
        let mut server = PeerClientServer::default();
        let front_id = parse_front_id(&server.handle_line("front-new").expect("front-new"));
        let set = format!(
            "front-set\t{front_id}\t{}\t{}",
            hex_text("trades/demo/item/state"),
            hex::encode(b"open")
        );
        assert_eq!(
            server.handle_line(&set).expect("front-set"),
            "front-set".to_string()
        );
        let serve = format!("front-serve-tcp\t{front_id}\t{}", hex_text("127.0.0.1:0"));
        let addr = parse_front_addr(&server.handle_line(&serve).expect("front serve"));
        let mut client =
            blocking::Client::connect_tcp(&addr, "codex", "/", 65_536).expect("connect front");
        assert!(client.stat_path("trades/demo/item/state").is_ok());

        let remove = format!(
            "front-remove-subtree\t{front_id}\t{}",
            hex_text("trades/demo/item")
        );
        assert_eq!(
            server.handle_line(&remove).expect("front-remove-subtree"),
            "front-remove-subtree".to_string()
        );
        assert!(client.stat_path("trades/demo/item").is_err());

        let stop = format!("front-stop\t{front_id}");
        assert_eq!(
            server.handle_line(&stop).expect("front-stop"),
            "front-stop".to_string()
        );
    }

    #[test]
    fn front_commands_serve_event_log() {
        let mut server = PeerClientServer::default();
        let front_id = parse_front_id(&server.handle_line("front-new").expect("front-new"));
        let register = format!(
            "front-register-log\t{front_id}\t{}",
            hex_text("trades/demo/events")
        );
        assert_eq!(
            server.handle_line(&register).expect("front-register-log"),
            "front-register-log".to_string()
        );
        let append = format!(
            "front-append-event\t{front_id}\t{}\t{}",
            hex_text("trades/demo/events"),
            hex::encode(b"one\ntwo\n")
        );
        assert_eq!(
            server.handle_line(&append).expect("front-append-event"),
            "front-append-event".to_string()
        );
        let serve = format!("front-serve-tcp\t{front_id}\t{}", hex_text("127.0.0.1:0"));
        let addr = parse_front_addr(&server.handle_line(&serve).expect("front serve"));

        let mut client =
            blocking::Client::connect_tcp(&addr, "codex", "/", 65_536).expect("connect front");
        let stat = client.stat_path("trades/demo/events").expect("stat events");
        assert_eq!(stat.length, 8);
        let body = client.read_path("trades/demo/events").expect("read events");
        assert_eq!(body, b"one\ntwo\n");

        let stop = format!("front-stop\t{front_id}");
        assert_eq!(
            server.handle_line(&stop).expect("front-stop"),
            "front-stop".to_string()
        );
    }

    #[test]
    fn front_commands_roundtrip_rpc_request() {
        let mut server = PeerClientServer::default();
        let front_id = parse_front_id(&server.handle_line("front-new").expect("front-new"));
        let wait_timeout = format!("front-set-wait-timeout\t{front_id}\t5000");
        assert_eq!(
            server
                .handle_line(&wait_timeout)
                .expect("front-set-wait-timeout"),
            "front-set-wait-timeout".to_string()
        );
        let register = format!(
            "front-register-rpc\t{front_id}\t{}",
            hex_text("declaration")
        );
        assert_eq!(
            server.handle_line(&register).expect("front-register-rpc"),
            "front-register-rpc".to_string()
        );
        let serve = format!("front-serve-tcp\t{front_id}\t{}", hex_text("127.0.0.1:0"));
        let addr = parse_front_addr(&server.handle_line(&serve).expect("front serve"));

        let client = thread::spawn(move || {
            let mut client =
                blocking::Client::connect_tcp(&addr, "codex", "/", 65_536).expect("connect front");
            client
                .rpc_path("declaration", b"compile this")
                .expect("rpc declaration")
        });

        let next = format!("front-next-request\t{front_id}\t1000");
        let request = server.handle_line(&next).expect("front-next-request");
        let fields = request.split('\t').collect::<Vec<_>>();
        assert_eq!(fields[0], "front-request");
        assert_eq!(
            hex::decode_text(fields[1]).expect("prefix"),
            "declaration".to_string()
        );
        assert_eq!(hex::decode(fields[3]).expect("body"), b"compile this");

        let complete = format!(
            "front-complete-request\t{front_id}\t{}\t{}\t{}",
            fields[1],
            fields[2],
            hex::encode(b"compiled")
        );
        assert_eq!(
            server
                .handle_line(&complete)
                .expect("front-complete-request"),
            "front-complete-request".to_string()
        );
        assert_eq!(client.join().expect("client join"), b"compiled");

        let stop = format!("front-stop\t{front_id}");
        assert_eq!(
            server.handle_line(&stop).expect("front-stop"),
            "front-stop".to_string()
        );
    }

    #[test]
    fn front_commands_roundtrip_remove_relay() {
        let mut server = PeerClientServer::default();
        let front_id = parse_front_id(&server.handle_line("front-new").expect("front-new"));
        let path = "trades/demo/stale";
        let set = format!(
            "front-set\t{front_id}\t{}\t{}",
            hex_text(path),
            hex::encode(b"stale")
        );
        assert_eq!(
            server.handle_line(&set).expect("front-set"),
            "front-set".to_string()
        );
        let register = format!(
            "front-register-remove-relay\t{front_id}\t{}",
            hex_text(path)
        );
        assert_eq!(
            server
                .handle_line(&register)
                .expect("front-register-remove-relay"),
            "front-register-remove-relay".to_string()
        );
        let serve = format!("front-serve-tcp\t{front_id}\t{}", hex_text("127.0.0.1:0"));
        let addr = parse_front_addr(&server.handle_line(&serve).expect("front serve"));

        let client = thread::spawn(move || {
            let mut client =
                blocking::Client::connect_tcp(&addr, "codex", "/", 65_536).expect("connect front");
            let fid = client.walk_path(path).expect("walk remove target");
            client.remove(fid)
        });

        let next = format!("front-next-request\t{front_id}\t1000");
        let request = server.handle_line(&next).expect("front-next-request");
        let fields = request.split('\t').collect::<Vec<_>>();
        assert_eq!(fields[0], "front-request");
        assert_eq!(hex::decode_text(fields[1]).expect("prefix"), path);
        assert_eq!(hex::decode(fields[3]).expect("body"), b"");

        let complete = format!(
            "front-complete-remove\t{front_id}\t{}\t{}",
            fields[1], fields[2]
        );
        assert_eq!(
            server
                .handle_line(&complete)
                .expect("front-complete-remove"),
            "front-complete-remove".to_string()
        );
        assert!(client.join().expect("client join").is_ok());

        let stop = format!("front-stop\t{front_id}");
        assert_eq!(
            server.handle_line(&stop).expect("front-stop"),
            "front-stop".to_string()
        );
    }

    fn temp_socket_path(label: &str) -> std::path::PathBuf {
        temp_directory_path(label).with_extension("sock")
    }

    fn temp_directory_path(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        env::temp_dir().join(format!(
            "r9p-beam-port-{label}-{}-{nanos}",
            std::process::id()
        ))
    }

    fn hex_text(value: &str) -> String {
        hex::encode(value.as_bytes())
    }

    fn parse_front_id(output: &str) -> u64 {
        let fields = output.split('\t').collect::<Vec<_>>();
        assert_eq!(fields[0], "front");
        fields[1].parse::<u64>().expect("front id")
    }

    fn parse_front_addr(output: &str) -> String {
        let fields = output.split('\t').collect::<Vec<_>>();
        assert_eq!(fields[0], "front-serve-tcp");
        hex::decode_text(fields[1]).expect("front address")
    }

    fn parse_authenticated_front_addr(output: &str) -> String {
        let fields = output.split('\t').collect::<Vec<_>>();
        assert_eq!(fields[0], "front-serve-tcp-authenticated");
        hex::decode_text(fields[1]).expect("authenticated front address")
    }

    fn target_fields(bind: &str) -> String {
        format!(
            "{}\t{}\t{}\t65536\t\t0",
            hex_text(bind),
            hex_text("codex"),
            hex_text("/")
        )
    }

    fn target_fields_with_auth(bind: &str, auth_config_path: &str) -> String {
        format!(
            "{}\t{}\t{}\t65536\t{}\t0",
            hex_text(bind),
            hex_text("codex"),
            hex_text("/"),
            hex_text(auth_config_path)
        )
    }
}
