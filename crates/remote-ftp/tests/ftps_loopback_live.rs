use localdesk_remote_ftp::{Credentials, FtpAdapter, FtpConfig, FtpFailureKind, RemotePath};
use std::{
    error::Error,
    fs,
    net::{Ipv4Addr, SocketAddrV4, TcpStream},
    num::NonZeroU16,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

const OPENSSL_PROGRAM: &str = "/usr/bin/openssl";
const PYTHON_PROGRAM: &str = "/usr/bin/python3";
const USERNAME: &str = "localdesk";
const PASSWORD: &str = "loopback-password";

struct ChildGuard {
    child: Child,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
#[ignore = "requires host Python and OpenSSL for an isolated explicit FTPS server"]
fn libcurl_validates_explicit_ftps_control_and_data_tls() -> Result<(), Box<dyn Error>> {
    require_executable(OPENSSL_PROGRAM)?;
    require_executable(PYTHON_PROGRAM)?;
    let workspace = TempDir::new()?;
    let certificate = workspace.path().join("certificate.pem");
    let key = workspace.path().join("key.pem");
    generate_certificate(&certificate, &key)?;
    let ready = workspace.path().join("ready");
    let events = workspace.path().join("events.log");
    let mut server = start_server(&certificate, &key, &ready, &events)?;
    let port = wait_for_server(&mut server.child, &ready)?;

    let untrusted = explicit_config(port)?;
    let error = FtpAdapter::new(untrusted)?.probe().unwrap_err();
    assert!(
        matches!(
            error,
            localdesk_remote_ftp::FtpError::Remote {
                failure: FtpFailureKind::Trust,
                ..
            }
        ),
        "self-signed endpoint did not fail as a trust error: {error:?}"
    );

    let mut trusted = explicit_config(port)?;
    trusted.ca_certificate_pem = Some(fs::read(&certificate)?);
    let adapter = FtpAdapter::new(trusted)?;
    adapter.probe()?;
    let root = RemotePath::root();
    let listing = String::from_utf8(adapter.list(&root)?)?;
    assert!(listing.lines().any(|line| {
        line.split_once(' ')
            .is_some_and(|(_, name)| name == "source.txt")
    }));

    let source = RemotePath::new("/source.txt")?;
    assert_eq!(adapter.stat_size(&source)?, Some(15));
    let (chunk, total) = adapter.read_chunk(&source, 4, 8)?;
    assert_eq!(chunk, b"back-sou");
    assert_eq!(total, 15);

    let local = workspace.path().join("upload.bin");
    fs::write(&local, b"explicit-ftps-upload")?;
    let temporary = RemotePath::new("/uploaded.txt.part")?;
    let final_path = RemotePath::new("/uploaded.txt")?;
    adapter.upload_with_temporary(&local, &temporary, &final_path, None)?;
    assert_eq!(adapter.stat_size(&final_path)?, Some(20));

    let resumed_payload = b"explicit-ftps-resume";
    let resumed_local = workspace.path().join("resumed.bin");
    fs::write(&resumed_local, resumed_payload)?;
    let resumed_temporary = RemotePath::new("/resumed.txt.part")?;
    let resumed_final = RemotePath::new("/resumed.txt")?;
    adapter.upload_with_temporary(&resumed_local, &resumed_temporary, &resumed_final, Some(8))?;
    let (resumed, resumed_size) = adapter.read_chunk(&resumed_final, 0, 64)?;
    assert_eq!(resumed, resumed_payload);
    assert_eq!(resumed_size, resumed_payload.len() as u64);

    let renamed = RemotePath::new("/renamed.txt")?;
    adapter.rename(&final_path, &renamed)?;
    adapter.delete_file(&renamed)?;
    let directory = RemotePath::new("/created")?;
    adapter.create_directory(&directory)?;
    adapter.remove_directory(&directory)?;

    assert_required_events(&events)?;
    assert!(
        server.child.try_wait()?.is_none(),
        "FTPS fixture exited early"
    );
    Ok(())
}

fn explicit_config(port: u16) -> Result<FtpConfig, Box<dyn Error>> {
    let port = NonZeroU16::new(port).ok_or("loopback port was zero")?;
    let mut config =
        FtpConfig::explicit_ftps("127.0.0.1", port, Credentials::new(USERNAME, PASSWORD)?)?;
    config.connect_timeout = Duration::from_secs(3);
    config.operation_timeout = Duration::from_secs(5);
    Ok(config)
}

fn generate_certificate(certificate: &Path, key: &Path) -> Result<(), Box<dyn Error>> {
    let output = Command::new(OPENSSL_PROGRAM)
        .args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-sha256",
            "-nodes",
            "-days",
            "1",
            "-subj",
            "/CN=127.0.0.1",
            "-addext",
            "subjectAltName=IP:127.0.0.1",
            "-keyout",
        ])
        .arg(key)
        .arg("-out")
        .arg(certificate)
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "openssl certificate generation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into())
    }
}

fn start_server(
    certificate: &Path,
    key: &Path,
    ready: &Path,
    events: &Path,
) -> Result<ChildGuard, Box<dyn Error>> {
    let script =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ftps_loopback_server.py");
    let child = Command::new(PYTHON_PROGRAM)
        .arg(script)
        .arg("--certificate")
        .arg(certificate)
        .arg("--key")
        .arg(key)
        .arg("--ready")
        .arg(ready)
        .arg("--events")
        .arg(events)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()?;
    Ok(ChildGuard { child })
}

fn wait_for_server(child: &mut Child, ready: &Path) -> Result<u16, Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            let stderr = child.stderr.take().map_or_else(String::new, |stderr| {
                std::io::read_to_string(stderr).unwrap_or_default()
            });
            return Err(format!("FTPS fixture exited with {status}: {stderr}").into());
        }
        if let Ok(value) = fs::read_to_string(ready) {
            let port = value.parse::<u16>()?;
            let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
            TcpStream::connect_timeout(&address.into(), Duration::from_millis(100))?;
            return Ok(port);
        }
        thread::sleep(Duration::from_millis(20));
    }
    Err("FTPS fixture did not become ready".into())
}

fn assert_required_events(path: &Path) -> Result<(), Box<dyn Error>> {
    let contents = fs::read_to_string(path)?;
    for required in [
        "REPLY 220",
        "AUTH TLS",
        "REPLY 234 AUTH TLS",
        "CONTROL_TLS",
        "PBSZ 0",
        "REPLY 200 PBSZ 0",
        "PROT P",
        "REPLY 200 PROT P",
        "DATA_TLS MLSD",
        "REST",
        "DATA_TLS RETR",
        "DATA_TLS STOR",
        "APPE",
        "DATA_TLS APPE",
    ] {
        assert!(
            contents.lines().any(|line| line == required),
            "missing FTPS event {required:?}: {contents}"
        );
    }
    assert!(
        !contents.lines().any(|line| line.starts_with("ERROR ")),
        "FTPS fixture reported an error: {contents}"
    );
    Ok(())
}

fn require_executable(path: &str) -> Result<(), Box<dyn Error>> {
    if Path::new(path).is_file() {
        Ok(())
    } else {
        Err(format!("required executable is missing: {path}").into())
    }
}
