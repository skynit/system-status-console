use localdesk_remote_core::{
    AdapterFuture, Authentication, BeginWriteRequest, CapabilityStatus, DataConnectionMode,
    EntryKind, FileOperation, ObjectIdentity, ProfileId, ProfileOptions, RemoteConnectionProfile,
    RemoteEndpoint, RemoteErrorKind, RemoteFileAdapter, RemoteFileSession, RemotePath,
    RemoteProtocol, RemoteReadRequest, SafeReason, SecretRef, SecretStore, SecretStoreError,
    SecretValue, TrustPolicy,
};
use localdesk_remote_ftp::{PLAIN_FTP_ACKNOWLEDGEMENT, PlainFtpConfirmation, RemoteFtpAdapter};
use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt::Write as _,
    future::Future,
    io::{self, BufRead, BufReader, Read, Write},
    net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const USERNAME: &str = "localdesk";
const PASSWORD: &str = "loopback-password";
const MAX_CONNECTIONS: usize = 64;
const MAX_COMMANDS_PER_CONNECTION: usize = 64;
const MAX_COMMAND_BYTES: usize = 4 * 1024;
const MAX_FILES: usize = 32;
const MAX_DATA_BYTES: usize = 1024 * 1024;
const CONTROL_IDLE_TIMEOUT: Duration = Duration::from_secs(1);
const IO_TIMEOUT: Duration = Duration::from_secs(3);

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

#[derive(Default)]
struct ServerState {
    directories: HashSet<String>,
    files: HashMap<String, Vec<u8>>,
    commands: Vec<String>,
    failures: Vec<String>,
}

struct LoopbackFtpServer {
    address: SocketAddr,
    state: Arc<Mutex<ServerState>>,
    stop: Arc<AtomicBool>,
    active_clients: Arc<AtomicUsize>,
    accept_thread: Option<thread::JoinHandle<()>>,
    client_threads: Arc<Mutex<Vec<thread::JoinHandle<()>>>>,
}

impl LoopbackFtpServer {
    fn start() -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let state = Arc::new(Mutex::new(ServerState {
            directories: HashSet::from(["/".to_owned(), "/workspace".to_owned()]),
            files: HashMap::from([("/source.txt".to_owned(), b"loopback-source".to_vec())]),
            commands: Vec::new(),
            failures: Vec::new(),
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let active_clients = Arc::new(AtomicUsize::new(0));
        let client_threads = Arc::new(Mutex::new(Vec::new()));

        let accept_state = Arc::clone(&state);
        let accept_stop = Arc::clone(&stop);
        let accept_active = Arc::clone(&active_clients);
        let accept_clients = Arc::clone(&client_threads);
        let accept_thread = thread::spawn(move || {
            let mut accepted = 0_usize;
            while !accept_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, peer)) => {
                        accepted += 1;
                        if accepted > MAX_CONNECTIONS || !peer.ip().is_loopback() {
                            let _ = reject_connection(stream);
                            continue;
                        }
                        let state = Arc::clone(&accept_state);
                        let active = Arc::clone(&accept_active);
                        active.fetch_add(1, Ordering::AcqRel);
                        let handle = thread::spawn(move || {
                            let result = serve_client(stream, &state);
                            if let Err(error) = result
                                && !matches!(
                                    error.kind(),
                                    io::ErrorKind::BrokenPipe
                                        | io::ErrorKind::ConnectionAborted
                                        | io::ErrorKind::ConnectionReset
                                )
                            {
                                state
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                                    .failures
                                    .push(error.to_string());
                            }
                            active.fetch_sub(1, Ordering::AcqRel);
                        });
                        accept_clients
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push(handle);
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => {
                        accept_state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .failures
                            .push(format!("accept failed: {error}"));
                        break;
                    }
                }
            }
        });

        Ok(Self {
            address,
            state,
            stop,
            active_clients,
            accept_thread: Some(accept_thread),
            client_threads,
        })
    }

    fn port(&self) -> u16 {
        self.address.port()
    }

    fn file(&self, path: &str) -> Option<Vec<u8>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .files
            .get(path)
            .cloned()
    }

    fn set_file(&self, path: &str, bytes: &[u8]) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .files
            .insert(path.to_owned(), bytes.to_vec());
    }

    fn command_count(&self, verb: &str) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .commands
            .iter()
            .filter(|actual| actual.as_str() == verb)
            .count()
    }

    fn directory_exists(&self, path: &str) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .directories
            .contains(path)
    }

    fn assert_clean(&self) -> Result<(), Box<dyn Error>> {
        let deadline = Instant::now() + IO_TIMEOUT;
        while self.active_clients.load(Ordering::Acquire) != 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let active_clients = self.active_clients.load(Ordering::Acquire);
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if active_clients == 0 && state.failures.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "loopback FTP server active_clients={active_clients}, failures={:?}",
                state.failures
            )
            .into())
        }
    }

    fn assert_commands(&self, required: &[&str]) -> Result<(), Box<dyn Error>> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let missing = required
            .iter()
            .copied()
            .filter(|verb| !state.commands.iter().any(|actual| actual == verb))
            .collect::<Vec<_>>();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "loopback FTP server did not observe {missing:?}; commands={:?}",
                state.commands
            )
            .into())
        }
    }
}

impl Drop for LoopbackFtpServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect_timeout(&self.address, Duration::from_millis(100));
        if let Some(handle) = self.accept_thread.take() {
            let _ = handle.join();
        }
        let handles = std::mem::take(
            &mut *self
                .client_threads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for handle in handles {
            let _ = handle.join();
        }
    }
}

#[test]
#[ignore = "runs the production libcurl adapter against an isolated loopback FTP server"]
fn libcurl_file_operations_work_against_an_isolated_loopback_server() -> Result<(), Box<dyn Error>>
{
    let server = LoopbackFtpServer::start()?;
    let reference = SecretRef::secret_service(ProfileId::new().as_uuid());
    let secrets = SingleSecret {
        reference: reference.clone(),
        value: PASSWORD.as_bytes().to_vec(),
    };
    let profile = RemoteConnectionProfile::new(
        ProfileId::new(),
        "loopback FTP",
        RemoteProtocol::Ftp,
        RemoteEndpoint::new("127.0.0.1", server.port())?,
        Some(USERNAME.to_owned()),
        None,
        Authentication::Password { secret: reference },
        TrustPolicy::PlaintextAcknowledged,
        ProfileOptions::Ftp {
            data_connection: DataConnectionMode::Passive,
        },
    )?;
    let confirmation = PlainFtpConfirmation::acknowledge(PLAIN_FTP_ACKNOWLEDGEMENT)?;
    let adapter = RemoteFtpAdapter::plain_ftp(confirmation);
    assert_eq!(
        adapter.capabilities().status(FileOperation::ResumeRead),
        &CapabilityStatus::Supported
    );
    assert_eq!(
        adapter.capabilities().status(FileOperation::ResumeWrite),
        &CapabilityStatus::Supported
    );
    let control =
        localdesk_remote_core::RemoteIoControl::new(Instant::now() + Duration::from_secs(30));
    let session = block_on(adapter.connect_controlled(&profile, &secrets, control.clone()))?;

    let root = RemotePath::new("/")?;
    let entries = block_on(session.list(&root))?;
    assert!(
        entries.iter().any(|entry| entry.name == "source.txt"),
        "loopback root did not contain source.txt: {entries:#?}"
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry.name == "workspace" && entry.kind == EntryKind::Directory),
        "loopback root did not report workspace as a directory: {entries:#?}"
    );
    assert!(block_on(session.list(&RemotePath::new("/workspace")?))?.is_empty());

    let source = RemotePath::new("/source.txt")?;
    let stat = block_on(session.stat(&source))?;
    assert_eq!(stat.identity.size_bytes, Some(15));
    let chunk = block_on(session.read_chunk(RemoteReadRequest {
        path: source,
        offset: 4,
        max_bytes: 8,
        expected_identity: Some(ObjectIdentity {
            size_bytes: Some(15),
            modified_at_unix_ms: None,
            etag: None,
        }),
    }))?;
    assert_eq!(chunk.bytes, b"back-sou");
    assert!(!chunk.eof);

    let final_path = RemotePath::new("/uploaded.txt")?;
    let temporary_path = RemotePath::new("/uploaded.txt.part")?;
    let payload = b"loopback-upload".to_vec();
    let started = block_on(session.begin_write_controlled(
        BeginWriteRequest {
            final_path: final_path.clone(),
            temporary_path,
            expected_size_bytes: Some(payload.len() as u64),
            resume_from: None,
            expected_destination: None,
        },
        control.clone(),
    ))
    .map_err(|error| io::Error::other(format!("begin_write: {error:?}")))?;
    let written = block_on(session.write_chunk_controlled(
        started.handle,
        0,
        payload.clone(),
        control.clone(),
    ))
    .map_err(|error| io::Error::other(format!("write_chunk: {error:?}")))?;
    assert_eq!(written.next_offset, payload.len() as u64);
    let committed =
        block_on(session.commit_write_controlled(started.handle, None, control.clone()))
            .map_err(|error| io::Error::other(format!("commit_write: {error:?}")))?;
    assert_eq!(committed.path, final_path);
    assert_eq!(server.file("/uploaded.txt"), Some(payload.clone()));

    assert_resume_write(session.as_ref(), &server)?;
    assert_resume_size_drift_rejected(session.as_ref(), &server)?;

    let renamed = RemotePath::new("/renamed.txt")?;
    block_on(session.rename(&final_path, &renamed))?;
    assert_eq!(server.file("/renamed.txt"), Some(payload));
    block_on(session.delete(&renamed))?;
    assert_eq!(server.file("/renamed.txt"), None);

    let directory = RemotePath::new("/created")?;
    block_on(session.create_directory(&directory))?;
    assert!(server.directory_exists("/created"));
    block_on(session.delete(&directory))?;
    assert!(!server.directory_exists("/created"));
    block_on(session.disconnect_controlled(control))?;
    server.assert_commands(&[
        "USER", "PASS", "NOOP", "EPSV", "MLSD", "SIZE", "REST", "RETR", "ABOR", "STOR", "RNFR",
        "APPE", "RNTO", "DELE", "MKD", "RMD",
    ])?;
    server.assert_clean()?;
    Ok(())
}

fn assert_resume_write(
    session: &dyn RemoteFileSession,
    server: &LoopbackFtpServer,
) -> Result<(), Box<dyn Error>> {
    let resumed_payload = b"resume-upload-complete".to_vec();
    let resume_offset = 7_u64;
    server.set_file(
        "/resumed.txt.part",
        &resumed_payload[..usize::try_from(resume_offset)?],
    );
    let resumed_path = RemotePath::new("/resumed.txt")?;
    let resumed_part = RemotePath::new("/resumed.txt.part")?;
    let resumed = block_on(session.begin_write(BeginWriteRequest {
        final_path: resumed_path.clone(),
        temporary_path: resumed_part,
        expected_size_bytes: Some(resumed_payload.len() as u64),
        resume_from: Some(resume_offset),
        expected_destination: None,
    }))?;
    let resumed_write = block_on(session.write_chunk(
        resumed.handle,
        resume_offset,
        resumed_payload[usize::try_from(resume_offset)?..].to_vec(),
    ))?;
    assert_eq!(resumed_write.next_offset, resumed_payload.len() as u64);
    let resumed_entry = block_on(session.commit_write(resumed.handle, None))?;
    assert_eq!(resumed_entry.path, resumed_path);
    assert_eq!(server.file("/resumed.txt"), Some(resumed_payload));
    Ok(())
}

fn assert_resume_size_drift_rejected(
    session: &dyn RemoteFileSession,
    server: &LoopbackFtpServer,
) -> Result<(), Box<dyn Error>> {
    server.set_file("/drifted.txt.part", b"bad");
    let appe_before_drift = server.command_count("APPE");
    let drift_error = block_on(session.begin_write(BeginWriteRequest {
        final_path: RemotePath::new("/drifted.txt")?,
        temporary_path: RemotePath::new("/drifted.txt.part")?,
        expected_size_bytes: Some(10),
        resume_from: Some(5),
        expected_destination: None,
    }))
    .expect_err("mismatched remote .part size must reject resume");
    assert_eq!(drift_error.kind, RemoteErrorKind::Conflict);
    assert_eq!(
        drift_error.reason.as_str(),
        "ftp_resume_partial_size_mismatch"
    );
    assert_eq!(server.command_count("APPE"), appe_before_drift);
    assert_eq!(server.file("/drifted.txt"), None);
    assert_eq!(server.file("/drifted.txt.part"), Some(b"bad".to_vec()));
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn serve_client(stream: TcpStream, state: &Arc<Mutex<ServerState>>) -> io::Result<()> {
    stream.set_read_timeout(Some(CONTROL_IDLE_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let mut reader = BufReader::new(stream);
    reply(&mut reader, "220 LocalDesk loopback FTP ready")?;
    let mut authenticated = false;
    let mut accepted_user = false;
    let mut cwd = "/".to_owned();
    let mut passive = None;
    let mut restart = 0_u64;
    let mut rename_from = None;

    for _ in 0..MAX_COMMANDS_PER_CONNECTION {
        let Some(line) = read_command(&mut reader)? else {
            return Ok(());
        };
        let (verb, argument) = split_command(&line);
        state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .commands
            .push(verb.to_owned());

        if !authenticated && !matches!(verb, "USER" | "PASS" | "QUIT") {
            reply(&mut reader, "530 Authentication required")?;
            continue;
        }
        match verb {
            "USER" => {
                accepted_user = argument == USERNAME;
                reply(
                    &mut reader,
                    if accepted_user {
                        "331 Password required"
                    } else {
                        "530 Authentication failed"
                    },
                )?;
            }
            "PASS" => {
                authenticated = accepted_user && argument == PASSWORD;
                reply(
                    &mut reader,
                    if authenticated {
                        "230 Login successful"
                    } else {
                        "530 Authentication failed"
                    },
                )?;
            }
            "SYST" => reply(&mut reader, "215 UNIX Type: L8")?,
            "FEAT" => reply_multiline(
                &mut reader,
                &["211-Features", " EPSV", " SIZE", " REST STREAM", "211 End"],
            )?,
            "PWD" | "XPWD" => reply(&mut reader, &format!("257 \"{cwd}\""))?,
            "CWD" => {
                let path = normalize_path(&cwd, argument)?;
                if state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .directories
                    .contains(&path)
                {
                    cwd = path;
                    reply(&mut reader, "250 Directory changed")?;
                } else {
                    reply(&mut reader, "550 Directory unavailable")?;
                }
            }
            "CDUP" => {
                cwd = parent_path(&cwd).to_owned();
                reply(&mut reader, "250 Directory changed")?;
            }
            "TYPE" => reply(&mut reader, "200 Type set")?,
            "OPTS" | "CLNT" => reply(&mut reader, "200 Option accepted")?,
            "NOOP" => reply(&mut reader, "200 NOOP ok")?,
            "ABOR" => {
                passive = None;
                restart = 0;
                reply(&mut reader, "226 Abort complete")?;
            }
            "EPSV" => passive = Some(enter_passive(&mut reader, true)?),
            "PASV" => passive = Some(enter_passive(&mut reader, false)?),
            "MLSD" => {
                let directory = normalize_path(&cwd, argument)?;
                let listing = machine_list_directory(state, &directory)?;
                send_data(&mut reader, passive.take(), listing.as_bytes())?;
            }
            "SIZE" => {
                let path = normalize_path(&cwd, argument)?;
                let size = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .files
                    .get(&path)
                    .map(Vec::len);
                match size {
                    Some(size) => reply(&mut reader, &format!("213 {size}"))?,
                    None => reply(&mut reader, "550 File unavailable")?,
                }
            }
            "REST" => match argument.parse::<u64>() {
                Ok(offset) if offset <= MAX_DATA_BYTES as u64 => {
                    restart = offset;
                    reply(&mut reader, "350 Restart position accepted")?;
                }
                _ => reply(&mut reader, "501 Invalid restart position")?,
            },
            "RETR" => {
                let path = normalize_path(&cwd, argument)?;
                let bytes = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .files
                    .get(&path)
                    .cloned();
                match bytes {
                    Some(bytes) if restart <= bytes.len() as u64 => {
                        let offset = usize::try_from(restart).map_err(io::Error::other)?;
                        send_data(&mut reader, passive.take(), &bytes[offset..])?;
                    }
                    _ => reply(&mut reader, "550 File unavailable")?,
                }
                restart = 0;
            }
            "STOR" | "APPE" => {
                let path = normalize_path(&cwd, argument)?;
                let bytes = receive_data(&mut reader, passive.take())?;
                let mut locked = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if !locked.files.contains_key(&path) && locked.files.len() >= MAX_FILES {
                    return Err(io::Error::other("file limit exceeded"));
                }
                if verb == "APPE" {
                    let existing = locked.files.entry(path).or_default();
                    if existing.len().saturating_add(bytes.len()) > MAX_DATA_BYTES {
                        return Err(io::Error::other("file byte limit exceeded"));
                    }
                    existing.extend_from_slice(&bytes);
                } else {
                    locked.files.insert(path, bytes);
                }
            }
            "MKD" | "XMKD" => {
                let path = normalize_path(&cwd, argument)?;
                let mut locked = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if locked.directories.len() >= MAX_FILES || !locked.directories.insert(path.clone())
                {
                    reply(&mut reader, "550 Directory unavailable")?;
                } else {
                    reply(&mut reader, &format!("257 \"{path}\" created"))?;
                }
            }
            "RMD" | "XRMD" => {
                let path = normalize_path(&cwd, argument)?;
                let mut locked = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let empty = !locked.files.keys().any(|item| parent_path(item) == path)
                    && !locked
                        .directories
                        .iter()
                        .any(|item| item != &path && parent_path(item) == path);
                if path != "/" && empty && locked.directories.remove(&path) {
                    reply(&mut reader, "250 Directory removed")?;
                } else {
                    reply(&mut reader, "550 Directory unavailable")?;
                }
            }
            "DELE" => {
                let path = normalize_path(&cwd, argument)?;
                if state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .files
                    .remove(&path)
                    .is_some()
                {
                    reply(&mut reader, "250 File deleted")?;
                } else {
                    reply(&mut reader, "550 File unavailable")?;
                }
            }
            "RNFR" => {
                let path = normalize_path(&cwd, argument)?;
                let locked = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if locked.files.contains_key(&path) || locked.directories.contains(&path) {
                    rename_from = Some(path);
                    reply(&mut reader, "350 Rename source accepted")?;
                } else {
                    reply(&mut reader, "550 Path unavailable")?;
                }
            }
            "RNTO" => {
                let Some(from) = rename_from.take() else {
                    reply(&mut reader, "503 RNFR required")?;
                    continue;
                };
                let to = normalize_path(&cwd, argument)?;
                let mut locked = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(bytes) = locked.files.remove(&from) {
                    locked.files.insert(to, bytes);
                    reply(&mut reader, "250 Rename complete")?;
                } else if locked.directories.remove(&from) {
                    locked.directories.insert(to);
                    reply(&mut reader, "250 Rename complete")?;
                } else {
                    reply(&mut reader, "550 Path unavailable")?;
                }
            }
            "QUIT" => {
                reply(&mut reader, "221 Goodbye")?;
                let _ = reader.get_mut().shutdown(Shutdown::Both);
                return Ok(());
            }
            _ => {
                reply(&mut reader, "502 Command not implemented")?;
                return Err(io::Error::other(format!("unexpected FTP command: {verb}")));
            }
        }
    }
    Err(io::Error::other("command limit exceeded"))
}

fn reject_connection(mut stream: TcpStream) -> io::Result<()> {
    stream.write_all(b"421 Too many connections\r\n")
}

fn read_command(reader: &mut BufReader<TcpStream>) -> io::Result<Option<String>> {
    let mut bytes = Vec::new();
    let read = match reader
        .by_ref()
        .take((MAX_COMMAND_BYTES + 1) as u64)
        .read_until(b'\n', &mut bytes)
    {
        Ok(read) => read,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    if read == 0 {
        return Ok(None);
    }
    if bytes.len() > MAX_COMMAND_BYTES || !bytes.ends_with(b"\n") {
        return Err(io::Error::other("FTP command line limit exceeded"));
    }
    while bytes
        .last()
        .is_some_and(|byte| matches!(byte, b'\r' | b'\n'))
    {
        bytes.pop();
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| io::Error::other("FTP command was not UTF-8"))
}

fn split_command(line: &str) -> (&str, &str) {
    let (verb, argument) = line.split_once(' ').unwrap_or((line, ""));
    (verb, argument.trim())
}

fn reply(reader: &mut BufReader<TcpStream>, line: &str) -> io::Result<()> {
    reader.get_mut().write_all(line.as_bytes())?;
    reader.get_mut().write_all(b"\r\n")?;
    reader.get_mut().flush()
}

fn reply_multiline(reader: &mut BufReader<TcpStream>, lines: &[&str]) -> io::Result<()> {
    for line in lines {
        reply(reader, line)?;
    }
    Ok(())
}

fn enter_passive(reader: &mut BufReader<TcpStream>, extended: bool) -> io::Result<TcpListener> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    if extended {
        reply(
            reader,
            &format!("229 Entering Extended Passive Mode (|||{port}|)"),
        )?;
    } else {
        let high = port / 256;
        let low = port % 256;
        reply(
            reader,
            &format!("227 Entering Passive Mode (127,0,0,1,{high},{low})"),
        )?;
    }
    Ok(listener)
}

fn accept_data(listener: Option<TcpListener>) -> io::Result<TcpStream> {
    let listener = listener.ok_or_else(|| io::Error::other("passive mode was not entered"))?;
    let deadline = Instant::now() + IO_TIMEOUT;
    loop {
        match listener.accept() {
            Ok((stream, peer)) if peer.ip().is_loopback() => {
                stream.set_read_timeout(Some(IO_TIMEOUT))?;
                stream.set_write_timeout(Some(IO_TIMEOUT))?;
                return Ok(stream);
            }
            Ok((stream, _)) => {
                let _ = stream.shutdown(Shutdown::Both);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "passive data connection timed out",
                    ));
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(error),
        }
    }
}

fn send_data(
    reader: &mut BufReader<TcpStream>,
    listener: Option<TcpListener>,
    bytes: &[u8],
) -> io::Result<()> {
    if bytes.len() > MAX_DATA_BYTES {
        return Err(io::Error::other("data byte limit exceeded"));
    }
    reply(reader, "150 Opening data connection")?;
    let mut stream = accept_data(listener)?;
    match stream.write_all(bytes) {
        Ok(()) => {}
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
            ) => {}
        Err(error) => return Err(error),
    }
    let _ = stream.shutdown(Shutdown::Both);
    reply(reader, "226 Transfer complete")
}

fn receive_data(
    reader: &mut BufReader<TcpStream>,
    listener: Option<TcpListener>,
) -> io::Result<Vec<u8>> {
    reply(reader, "150 Opening data connection")?;
    let mut stream = accept_data(listener)?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut stream)
        .take((MAX_DATA_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_DATA_BYTES {
        return Err(io::Error::other("data byte limit exceeded"));
    }
    reply(reader, "226 Transfer complete")?;
    Ok(bytes)
}

fn machine_list_directory(state: &Arc<Mutex<ServerState>>, directory: &str) -> io::Result<String> {
    let locked = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !locked.directories.contains(directory) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "listing directory does not exist",
        ));
    }
    let mut entries = locked
        .directories
        .iter()
        .filter(|path| path.as_str() != directory && parent_path(path) == directory)
        .filter_map(|path| path.rsplit('/').next().map(|name| (name, None)))
        .chain(locked.files.iter().filter_map(|(path, bytes)| {
            (parent_path(path) == directory)
                .then(|| {
                    path.rsplit('/')
                        .next()
                        .map(|name| (name, Some(bytes.len())))
                })
                .flatten()
        }))
        .collect::<Vec<_>>();
    entries.sort_unstable_by_key(|(name, _)| *name);
    let mut listing = String::new();
    for (name, size) in entries {
        if let Some(size) = size {
            writeln!(listing, "type=file;size={size}; {name}\r")
                .expect("writing to String cannot fail");
        } else {
            writeln!(listing, "type=dir; {name}\r").expect("writing to String cannot fail");
        }
    }
    Ok(listing)
}

fn normalize_path(cwd: &str, argument: &str) -> io::Result<String> {
    let combined = if argument.is_empty() {
        cwd.to_owned()
    } else if argument.starts_with('/') {
        argument.to_owned()
    } else if cwd == "/" {
        format!("/{argument}")
    } else {
        format!("{cwd}/{argument}")
    };
    let mut components = Vec::new();
    for component in combined.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop();
            }
            value if value.chars().any(char::is_control) => {
                return Err(io::Error::other("path contains control characters"));
            }
            value => components.push(value),
        }
    }
    if components.is_empty() {
        Ok("/".to_owned())
    } else {
        Ok(format!("/{}", components.join("/")))
    }
}

fn parent_path(path: &str) -> &str {
    path.rsplit_once('/').map_or(
        "/",
        |(parent, _)| {
            if parent.is_empty() { "/" } else { parent }
        },
    )
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
