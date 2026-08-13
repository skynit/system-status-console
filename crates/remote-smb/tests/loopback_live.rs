use localdesk_remote_core::{
    AdapterFuture, Authentication, BeginWriteRequest, CapabilityStatus, FileOperation, ProfileId,
    ProfileOptions, RemoteConnectionProfile, RemoteEndpoint, RemoteErrorKind, RemoteFileAdapter,
    RemoteFileSession, RemotePath, RemoteProtocol, RemoteReadRequest, SafeReason, SecretRef,
    SecretStore, SecretStoreError, SecretValue, SmbDialect, TrustPolicy,
};
use localdesk_remote_smb::SmbRemoteFileAdapter;
use std::{
    error::Error,
    fs::{self, File},
    future::Future,
    io::{Read, Write},
    net::{Ipv4Addr, TcpListener},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

const SMBD_PROGRAM: &str = "/usr/sbin/smbd";
const PDBEDIT_PROGRAM: &str = "/usr/sbin/pdbedit";
const ID_PROGRAM: &str = "/usr/bin/id";
const SHARE_NAME: &str = "localdesk";

struct SingleSecret {
    reference: SecretRef,
    value: Vec<u8>,
}

impl SecretStore for SingleSecret {
    fn resolve<'a>(
        &'a self,
        reference: &'a SecretRef,
    ) -> AdapterFuture<'a, Result<SecretValue, SecretStoreError>> {
        let result = if reference == &self.reference {
            Ok(SecretValue::new(self.value.clone()))
        } else {
            Err(SecretStoreError::NotFound(
                SafeReason::new("loopback_secret_not_found").expect("static reason"),
            ))
        };
        Box::pin(async move { result })
    }

    fn delete<'a>(
        &'a self,
        _reference: &'a SecretRef,
    ) -> AdapterFuture<'a, Result<(), SecretStoreError>> {
        Box::pin(async { Ok(()) })
    }
}

struct TempWorkspace {
    path: PathBuf,
}

impl TempWorkspace {
    fn new() -> Result<Self, Box<dyn Error>> {
        let suffix = Uuid::new_v4().simple().to_string();
        let path = PathBuf::from("/tmp").join(format!("ldsmb-{}", &suffix[..12]));
        fs::create_dir(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct SmbdGuard {
    child: Child,
}

impl Drop for SmbdGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
#[ignore = "requires the host Samba smbd, pdbedit, and libsmbclient"]
fn libsmbclient_file_operations_work_against_an_isolated_loopback_server()
-> Result<(), Box<dyn Error>> {
    require_executable(SMBD_PROGRAM)?;
    require_executable(PDBEDIT_PROGRAM)?;
    require_executable(ID_PROGRAM)?;

    let workspace = TempWorkspace::new()?;
    let share = workspace.path().join("share");
    fs::create_dir(&share)?;
    fs::write(share.join("source.txt"), b"loopback-source")?;
    let username = current_username()?;
    let password = format!("localdesk-loopback-{}", Uuid::new_v4());
    let port = free_loopback_port()?;
    let config = write_config(workspace.path(), &share, port)?;
    create_samba_password(&config, &username, &password)?;
    let _smbd = start_smbd(workspace.path(), &config, port)?;

    let reference = SecretRef::secret_service(ProfileId::new().as_uuid());
    let secrets = SingleSecret {
        reference: reference.clone(),
        value: password.into_bytes(),
    };
    let profile = RemoteConnectionProfile::new(
        ProfileId::new(),
        "loopback SMB",
        RemoteProtocol::Smb,
        RemoteEndpoint::new("127.0.0.1", port)?,
        Some(username),
        None,
        Authentication::Password { secret: reference },
        TrustPolicy::SmbNegotiated,
        ProfileOptions::Smb {
            share: Some(SHARE_NAME.to_owned()),
            minimum_dialect: SmbDialect::Smb2,
            require_signing: false,
            require_encryption: false,
        },
    )?;

    let adapter = SmbRemoteFileAdapter::system();
    assert_eq!(
        adapter.capabilities().status(FileOperation::ResumeRead),
        &CapabilityStatus::Supported
    );
    assert_eq!(
        adapter.capabilities().status(FileOperation::ResumeWrite),
        &CapabilityStatus::Supported
    );
    let session = block_on(adapter.connect(&profile, &secrets))?;
    let root = RemotePath::new("/")?;
    let entries = block_on(session.list(&root))?;
    assert!(
        entries.iter().any(|entry| entry.name == "source.txt"),
        "loopback share did not contain source.txt: {entries:#?}"
    );

    let source = RemotePath::new("/source.txt")?;
    let source_identity = block_on(session.stat(&source))?.identity;
    let chunk = block_on(session.read_chunk(RemoteReadRequest {
        path: source,
        offset: 4,
        max_bytes: 8,
        expected_identity: Some(source_identity),
    }))?;
    assert_eq!(chunk.bytes, b"back-sou");
    assert!(!chunk.eof);

    let final_path = RemotePath::new("/uploaded.txt")?;
    let temporary_path = RemotePath::new("/uploaded.txt.part")?;
    let payload = b"loopback-upload".to_vec();
    let started = block_on(session.begin_write(BeginWriteRequest {
        final_path: final_path.clone(),
        temporary_path,
        expected_size_bytes: Some(payload.len() as u64),
        resume_from: None,
        expected_destination: None,
    }))
    .map_err(|error| std::io::Error::other(format!("begin_write: {error:?}")))?;
    let written = block_on(session.write_chunk(started.handle, 0, payload.clone()))
        .map_err(|error| std::io::Error::other(format!("write_chunk: {error:?}")))?;
    assert_eq!(written.next_offset, payload.len() as u64);
    let committed = block_on(session.commit_write(started.handle, None))
        .map_err(|error| std::io::Error::other(format!("commit_write: {error:?}")))?;
    assert_eq!(committed.path, final_path);

    assert_resume_write(session.as_ref(), &share)?;
    assert_resume_size_drift_rejected(session.as_ref(), &share)?;

    let renamed = RemotePath::new("/renamed.txt")?;
    block_on(session.rename(&final_path, &renamed))?;
    assert_eq!(fs::read(share.join("renamed.txt"))?, payload);
    block_on(session.delete(&renamed))?;
    assert!(!share.join("renamed.txt").exists());

    let directory = RemotePath::new("/created")?;
    block_on(session.create_directory(&directory))?;
    assert!(share.join("created").is_dir());
    block_on(session.delete(&directory))?;
    assert!(!share.join("created").exists());
    block_on(session.disconnect())?;
    Ok(())
}

fn assert_resume_write(
    session: &dyn RemoteFileSession,
    share: &Path,
) -> Result<(), Box<dyn Error>> {
    let payload = b"resume-upload-complete";
    let offset = 7_u64;
    fs::write(
        share.join("resumed.txt.part"),
        &payload[..usize::try_from(offset)?],
    )?;
    let final_path = RemotePath::new("/resumed.txt")?;
    let started = block_on(session.begin_write(BeginWriteRequest {
        final_path: final_path.clone(),
        temporary_path: RemotePath::new("/resumed.txt.part")?,
        expected_size_bytes: Some(payload.len() as u64),
        resume_from: Some(offset),
        expected_destination: None,
    }))?;
    let written = block_on(session.write_chunk(
        started.handle,
        offset,
        payload[usize::try_from(offset)?..].to_vec(),
    ))?;
    assert_eq!(written.next_offset, payload.len() as u64);
    let committed = block_on(session.commit_write(started.handle, None))?;
    assert_eq!(committed.path, final_path);
    assert_eq!(fs::read(share.join("resumed.txt"))?, payload);
    Ok(())
}

fn assert_resume_size_drift_rejected(
    session: &dyn RemoteFileSession,
    share: &Path,
) -> Result<(), Box<dyn Error>> {
    fs::write(share.join("drifted.txt.part"), b"bad")?;
    let error = block_on(session.begin_write(BeginWriteRequest {
        final_path: RemotePath::new("/drifted.txt")?,
        temporary_path: RemotePath::new("/drifted.txt.part")?,
        expected_size_bytes: Some(10),
        resume_from: Some(5),
        expected_destination: None,
    }))
    .expect_err("mismatched SMB .part size must reject resume");
    assert_eq!(error.kind, RemoteErrorKind::Conflict);
    assert_eq!(error.reason.as_str(), "smb_resume_offset_mismatch");
    assert_eq!(fs::read(share.join("drifted.txt.part"))?, b"bad");
    assert!(!share.join("drifted.txt").exists());
    Ok(())
}

fn write_config(workspace: &Path, share: &Path, port: u16) -> Result<PathBuf, Box<dyn Error>> {
    for directory in [
        "private", "lock", "state", "cache", "pid", "logs", "ncalrpc",
    ] {
        fs::create_dir(workspace.join(directory))?;
    }
    let config = workspace.join("smb.conf");
    let contents = format!(
        "[global]\n\
         server role = standalone server\n\
         workgroup = WORKGROUP\n\
         security = user\n\
         map to guest = Never\n\
         interfaces = 127.0.0.1\n\
         bind interfaces only = yes\n\
         disable netbios = yes\n\
         smb ports = {port}\n\
         server min protocol = SMB2\n\
         server max protocol = SMB3\n\
         private dir = {}\n\
         lock directory = {}\n\
         state directory = {}\n\
         cache directory = {}\n\
         pid directory = {}\n\
         ncalrpc dir = {}\n\
         log file = /dev/stdout\n\
         max log size = 0\n\
         passdb backend = smbpasswd:{}\n\
         load printers = no\n\
         printing = bsd\n\
         printcap name = /dev/null\n\
         dns proxy = no\n\
         enable core files = no\n\
         unix password sync = no\n\
         pam password change = no\n\
         [localdesk]\n\
         path = {}\n\
         read only = no\n\
         browseable = yes\n\
         guest ok = no\n",
        workspace.join("private").display(),
        workspace.join("lock").display(),
        workspace.join("state").display(),
        workspace.join("cache").display(),
        workspace.join("pid").display(),
        workspace.join("ncalrpc").display(),
        workspace.join("private/smbpasswd").display(),
        share.display(),
    );
    fs::write(&config, contents)?;
    Ok(config)
}

fn create_samba_password(
    config: &Path,
    username: &str,
    password: &str,
) -> Result<(), Box<dyn Error>> {
    let mut child = Command::new(PDBEDIT_PROGRAM)
        .args(["--create", "--password-from-stdin", "--configfile"])
        .arg(config)
        .arg("--user")
        .arg(username)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or("pdbedit stdin unavailable")?;
    writeln!(stdin, "{password}")?;
    writeln!(stdin, "{password}")?;
    drop(stdin);
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(format!(
            "pdbedit failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(())
}

fn start_smbd(workspace: &Path, config: &Path, port: u16) -> Result<SmbdGuard, Box<dyn Error>> {
    let log_path = workspace.join("smbd.stdout.log");
    let log = File::create(&log_path)?;
    let mut child = Command::new(SMBD_PROGRAM)
        .args(["-i", "--debug-stdout", "-d", "3", "-s"])
        .arg(config)
        .arg("-p")
        .arg(port.to_string())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            return Err(format!(
                "loopback smbd exited early with {status}: {}",
                read_log(&log_path)
            )
            .into());
        }
        if TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_err() {
            return Ok(SmbdGuard { child });
        }
        thread::sleep(Duration::from_millis(25));
    }
    let _ = child.kill();
    let _ = child.wait();
    Err(format!("loopback smbd did not listen: {}", read_log(&log_path)).into())
}

fn free_loopback_port() -> Result<u16, Box<dyn Error>> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    Ok(listener.local_addr()?.port())
}

fn current_username() -> Result<String, Box<dyn Error>> {
    let output = Command::new(ID_PROGRAM).arg("-un").output()?;
    if !output.status.success() {
        return Err("id -un failed".into());
    }
    let username = String::from_utf8(output.stdout)?.trim().to_owned();
    if username.is_empty() {
        return Err("id -un returned an empty username".into());
    }
    Ok(username)
}

fn require_executable(path: &str) -> Result<(), Box<dyn Error>> {
    if Path::new(path).is_file() {
        Ok(())
    } else {
        Err(format!("required executable is missing: {path}").into())
    }
}

fn read_log(path: &Path) -> String {
    let mut contents = String::new();
    let _ = File::open(path).and_then(|mut file| file.read_to_string(&mut contents));
    contents
}

fn block_on<F: Future>(future: F) -> F::Output {
    use std::pin::Pin;
    use std::task::{Context, Poll};

    let mut context = Context::from_waker(std::task::Waker::noop());
    let mut future = Box::pin(future);
    loop {
        if let Poll::Ready(value) = Pin::new(&mut future).poll(&mut context) {
            return value;
        }
        thread::yield_now();
    }
}
