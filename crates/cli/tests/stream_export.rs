use std::{
    error::Error,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use r9p::export_descriptor::ExportDescriptor;
use r9p_auth::{Certificate, CertificateBody, KeyPair, RootKeyPair};

type TestResult<T> = Result<T, Box<dyn Error>>;

const SERVER_DOMAIN: &str = "stream-export.test";
const ALLOWED_PRINCIPAL: &str = "/srv/coordinator/nucbox";
const DENIED_PRINCIPAL: &str = "/srv/unrelated/service";

struct TestRoot {
    path: PathBuf,
}

impl TestRoot {
    fn new() -> TestResult<Self> {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path = std::env::temp_dir().join(format!(
            "r9p-stream-export-test-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path)?;
        Ok(Self { path })
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct ServerProcess(Child);

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn authenticated_stream_export_is_byte_transparent_and_principal_bounded() -> TestResult<()> {
    let root = TestRoot::new()?;
    let signing_root = r9p_auth::generate_root_key_pair()?;
    let server_config = write_server_config(&root.path, &signing_root)?;
    let allowed_config = write_client_config(&root.path, &signing_root, ALLOWED_PRINCIPAL)?;
    let denied_config = write_client_config(&root.path, &signing_root, DENIED_PRINCIPAL)?;
    let descriptor_file = root.path.join("stream.descriptor");
    let cat = find_executable("cat")?;

    let server = Command::new(env!("CARGO_BIN_EXE_r9p"))
        .args([
            "stream-export",
            "--bind",
            "127.0.0.1:0",
            "--auth-config",
            &server_config.to_string_lossy(),
            "--allow-principal",
            ALLOWED_PRINCIPAL,
            "--descriptor-file",
            &descriptor_file.to_string_lossy(),
            "--",
            &cat.to_string_lossy(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let _server = ServerProcess(server);
    let descriptor = wait_for_descriptor(&descriptor_file)?;

    let mut input = vec![0x00, 0xff, b'\r', b'\n', 0x1b];
    input.extend((0_u32..130_000).map(|value| value.wrapping_mul(31) as u8));
    let mut client = Command::new(env!("CARGO_BIN_EXE_r9p"))
        .args([
            "--auth-config",
            &allowed_config.to_string_lossy(),
            "--auth-domain",
            SERVER_DOMAIN,
            "--bind",
            &descriptor.endpoint_bind,
            "-u",
            ALLOWED_PRINCIPAL,
            "-A",
            "/",
            "stream",
            "/stream",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    client
        .stdin
        .take()
        .ok_or("stream client stdin unavailable")?
        .write_all(&input)?;
    let output = client.wait_with_output()?;
    if !output.status.success() {
        return Err(format!(
            "allowed stream failed status={:?} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    if output.stdout != input {
        return Err("stream exporter changed relayed bytes".into());
    }

    let denied = Command::new(env!("CARGO_BIN_EXE_r9p"))
        .args([
            "--auth-config",
            &denied_config.to_string_lossy(),
            "--auth-domain",
            SERVER_DOMAIN,
            "--bind",
            &descriptor.endpoint_bind,
            "-u",
            DENIED_PRINCIPAL,
            "-A",
            "/",
            "attach",
        ])
        .output()?;
    if denied.status.success() {
        return Err("unauthorized certified principal reached the stream export".into());
    }
    Ok(())
}

#[test]
fn git_fetch_uses_the_authenticated_stream_without_a_git_specific_adapter() -> TestResult<()> {
    let root = TestRoot::new()?;
    let signing_root = r9p_auth::generate_root_key_pair()?;
    let server_config = write_server_config(&root.path, &signing_root)?;
    let client_config = write_client_config(&root.path, &signing_root, ALLOWED_PRINCIPAL)?;
    let descriptor_file = root.path.join("git-stream.descriptor");
    let git = find_executable("git")?;
    let authority = root.path.join("authority");
    let standby = root.path.join("standby");
    fs::create_dir(&authority)?;
    fs::create_dir(&standby)?;

    run_git(&git, &authority, &["init", "-b", "main"])?;
    run_git(&git, &authority, &["config", "user.name", "Coordinator"])?;
    run_git(
        &git,
        &authority,
        &["config", "user.email", "coordinator@example.invalid"],
    )?;
    fs::write(authority.join("state"), "generation one\n")?;
    run_git(&git, &authority, &["add", "--", "state"])?;
    run_git(&git, &authority, &["commit", "-m", "Record generation one"])?;
    let first_head = git_output(&git, &authority, &["rev-parse", "HEAD"])?;

    let server = Command::new(env!("CARGO_BIN_EXE_r9p"))
        .args([
            "stream-export",
            "--bind",
            "127.0.0.1:0",
            "--auth-config",
            &server_config.to_string_lossy(),
            "--allow-principal",
            ALLOWED_PRINCIPAL,
            "--descriptor-file",
            &descriptor_file.to_string_lossy(),
            "--",
            &git.to_string_lossy(),
            "upload-pack",
            "--strict",
            &authority.join(".git").to_string_lossy(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let _server = ServerProcess(server);
    let descriptor = wait_for_descriptor(&descriptor_file)?;
    let remote = format!(
        "ext::{} --auth-config {} --auth-domain {} --bind {} -u {} -A / stream /stream",
        env!("CARGO_BIN_EXE_r9p"),
        client_config.display(),
        SERVER_DOMAIN,
        descriptor.endpoint_bind,
        ALLOWED_PRINCIPAL,
    );

    run_git(&git, &standby, &["init", "-b", "main"])?;
    fetch_replica(&git, &standby, &remote)?;
    assert_eq!(
        git_output(
            &git,
            &standby,
            &["rev-parse", "refs/coordinator-replica/fetched"],
        )?,
        first_head,
    );

    fs::write(authority.join("state"), "generation two\n")?;
    run_git(&git, &authority, &["add", "--", "state"])?;
    run_git(&git, &authority, &["commit", "-m", "Record generation two"])?;
    let second_head = git_output(&git, &authority, &["rev-parse", "HEAD"])?;
    fetch_replica(&git, &standby, &remote)?;
    assert_ne!(first_head, second_head);
    assert_eq!(
        git_output(
            &git,
            &standby,
            &["rev-parse", "refs/coordinator-replica/fetched"],
        )?,
        second_head,
    );
    run_git(
        &git,
        &standby,
        &["merge-base", "--is-ancestor", &first_head, &second_head],
    )?;
    Ok(())
}

fn write_server_config(root: &Path, signing_root: &RootKeyPair) -> TestResult<PathBuf> {
    let key = r9p_auth::generate_key_pair()?;
    let certificate = issue(signing_root, &key, SERVER_DOMAIN)?;
    let config = root.join("server.conf");
    write_identity(root, "server", &key, &certificate)?;
    fs::write(
        &config,
        format!(
            "format r9p-session-auth.v1\nrole server\ndomain {SERVER_DOMAIN}\nprivate-key {}\ncertificate {}\nroot {}\n",
            root.join("server.key").display(),
            root.join("server.crt").display(),
            signing_root.public
        ),
    )?;
    Ok(config)
}

fn write_client_config(
    root: &Path,
    signing_root: &RootKeyPair,
    principal: &str,
) -> TestResult<PathBuf> {
    let label = principal.trim_start_matches('/').replace(['/', '.'], "-");
    let key = r9p_auth::generate_key_pair()?;
    let certificate = issue(signing_root, &key, principal)?;
    let config = root.join(format!("{label}.conf"));
    write_identity(root, &label, &key, &certificate)?;
    fs::write(
        &config,
        format!(
            "format r9p-session-auth.v1\nrole client\nprivate-key {}\ncertificate {}\nroot {}\n",
            root.join(format!("{label}.key")).display(),
            root.join(format!("{label}.crt")).display(),
            signing_root.public
        ),
    )?;
    Ok(config)
}

fn write_identity(
    root: &Path,
    label: &str,
    key: &KeyPair,
    certificate: &Certificate,
) -> TestResult<()> {
    r9p_auth::write_key_pair(
        &root.join(format!("{label}.key")),
        &root.join(format!("{label}.pub")),
        key,
    )?;
    certificate.write(&root.join(format!("{label}.crt")))?;
    Ok(())
}

fn issue(signing_root: &RootKeyPair, key: &KeyPair, name: &str) -> TestResult<Certificate> {
    Ok(Certificate::sign(
        &signing_root.private,
        CertificateBody::new(
            name,
            key.public,
            Vec::<String>::new(),
            1,
            4_000_000_000,
            signing_root.public,
        )?,
    )?)
}

fn wait_for_descriptor(path: &Path) -> TestResult<ExportDescriptor> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match fs::read_to_string(path) {
            Ok(content) => return Ok(ExportDescriptor::parse(&content)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if Instant::now() >= deadline {
            return Err(format!("stream descriptor did not appear at {}", path.display()).into());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn find_executable(name: &str) -> TestResult<PathBuf> {
    let path = std::env::var_os("PATH").ok_or("PATH unavailable")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| format!("{name} not found on PATH").into())
}

fn fetch_replica(git: &Path, repository: &Path, remote: &str) -> TestResult<()> {
    run_git(
        git,
        repository,
        &[
            "-c",
            "protocol.ext.allow=always",
            "-c",
            "transfer.fsckObjects=true",
            "fetch",
            "--atomic",
            "--no-tags",
            remote,
            "refs/heads/main:refs/coordinator-replica/fetched",
        ],
    )
}

fn run_git(git: &Path, repository: &Path, arguments: &[&str]) -> TestResult<()> {
    let output = Command::new(git)
        .current_dir(repository)
        .args(arguments)
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "git {:?} failed status={:?} stderr={}",
            arguments,
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        )
        .into())
    }
}

fn git_output(git: &Path, repository: &Path, arguments: &[&str]) -> TestResult<String> {
    let output = Command::new(git)
        .current_dir(repository)
        .args(arguments)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "git {:?} failed status={:?} stderr={}",
            arguments,
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}
