use localdesk_remote_core::{
    AdapterFuture, Authentication, BeginWriteRequest, FirstUsePolicy, ProfileId, ProfileOptions,
    RemoteConnectionProfile, RemoteEndpoint, RemoteFileAdapter, RemotePath, RemoteProtocol,
    RemoteReadRequest, SafeReason, SecretRef, SecretStore, SecretStoreError, SecretValue,
    TrustPolicy,
};
use localdesk_remote_ssh::{
    Authentication as PrivateAuthentication, DisconnectReason, Endpoint as PrivateEndpoint,
    HostKeyPolicy, HostTrust, OpenSshAdapter, PtySize, SessionState, SftpOperation,
    SftpRemoteFileAdapter, SshProfile, SshTerminalAdapter, TerminalRead,
};
use std::{
    env,
    error::Error,
    fs::{self, File},
    io::Read,
    net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream},
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

const SSHD_PROGRAM: &str = "/usr/sbin/sshd";
const SSH_KEYGEN_PROGRAM: &str = "/usr/bin/ssh-keygen";
const ID_PROGRAM: &str = "/usr/bin/id";

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

struct MultipleSecrets(Vec<(SecretRef, Vec<u8>)>);

impl SecretStore for MultipleSecrets {
    fn resolve<'a>(
        &'a self,
        reference: &'a SecretRef,
    ) -> AdapterFuture<'a, Result<SecretValue, SecretStoreError>> {
        let result = self
            .0
            .iter()
            .find(|(candidate, _)| candidate == reference)
            .map(|(_, value)| SecretValue::new(value.clone()))
            .ok_or_else(|| {
                SecretStoreError::NotFound(
                    SafeReason::new("loopback_secret_not_found").expect("static reason"),
                )
            });
        Box::pin(async move { result })
    }

    fn delete<'a>(
        &'a self,
        _reference: &'a SecretRef,
    ) -> AdapterFuture<'a, Result<(), SecretStoreError>> {
        Box::pin(async { Ok(()) })
    }
}

struct SshdGuard {
    child: Child,
}

impl Drop for SshdGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the host OpenSSH client and sshd binaries"]
async fn openssh_terminal_and_sftp_work_against_an_isolated_loopback_server()
-> Result<(), Box<dyn Error>> {
    require_executable(SSHD_PROGRAM)?;
    require_executable(SSH_KEYGEN_PROGRAM)?;
    require_executable(ID_PROGRAM)?;

    let workspace = TempDir::new()?;
    let host_key = workspace.path().join("host_key");
    let client_key = workspace.path().join("client_key");
    generate_key(&host_key)?;
    generate_key(&client_key)?;

    let authorized_keys = workspace.path().join("authorized_keys");
    fs::copy(client_key.with_extension("pub"), &authorized_keys)?;
    let known_hosts = workspace.path().join("known_hosts");
    let port = free_loopback_port()?;
    fs::write(&known_hosts, [])?;

    let username = current_username()?;
    let log_path = workspace.path().join("sshd.log");
    let terminal_sshd = start_sshd(
        workspace.path(),
        &host_key,
        &authorized_keys,
        &log_path,
        port,
        Some("/bin/sh"),
    )?;

    let reference = SecretRef::secret_service(ProfileId::new().as_uuid());
    let secrets = SingleSecret {
        reference: reference.clone(),
        value: fs::read(&client_key)?,
    };
    let trust = HostTrust {
        known_hosts_file: known_hosts.clone(),
        revoked_host_keys_file: None,
        policy: HostKeyPolicy::Strict,
    };

    verify_first_use_confirmation(
        port,
        &username,
        &reference,
        &secrets,
        trust.clone(),
        &known_hosts,
    )
    .await?;
    verify_terminal(port, &username, &reference, &secrets, trust.clone()).await?;
    drop(terminal_sshd);
    let _sftp_sshd = start_sshd(
        workspace.path(),
        &host_key,
        &authorized_keys,
        &log_path,
        port,
        None,
    )?;
    verify_sftp(
        workspace.path(),
        port,
        &username,
        &reference,
        &secrets,
        trust,
    )
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the host OpenSSH client and sshd binaries"]
async fn encrypted_ed25519_terminal_and_sftp_use_sealed_askpass_without_secret_leaks()
-> Result<(), Box<dyn Error>> {
    require_executable(SSHD_PROGRAM)?;
    require_executable(SSH_KEYGEN_PROGRAM)?;
    require_executable(ID_PROGRAM)?;

    let workspace = TempDir::new()?;
    let host_key = workspace.path().join("host_key");
    let client_key = workspace.path().join("encrypted_client_key");
    let passphrase = b"localdesk-encrypted-key-loopback-secret";
    generate_key(&host_key)?;
    generate_key_with_passphrase(&client_key, passphrase)?;

    let authorized_keys = workspace.path().join("authorized_keys");
    fs::copy(client_key.with_extension("pub"), &authorized_keys)?;
    let known_hosts = workspace.path().join("known_hosts");
    fs::write(&known_hosts, [])?;
    let log_path = workspace.path().join("sshd.log");
    let terminal_port = free_loopback_port()?;
    let sftp_port = free_loopback_port()?;
    let username = current_username()?;
    let _terminal_sshd = start_sshd(
        workspace.path(),
        &host_key,
        &authorized_keys,
        &log_path,
        terminal_port,
        Some("/bin/sh"),
    )?;
    let sftp_log_path = workspace.path().join("sftp-sshd.log");
    let _sftp_sshd = start_sshd(
        workspace.path(),
        &host_key,
        &authorized_keys,
        &sftp_log_path,
        sftp_port,
        None,
    )?;
    append_known_host(&known_hosts, &host_key.with_extension("pub"), sftp_port)?;

    let key_reference = SecretRef::secret_service(ProfileId::new().as_uuid());
    let passphrase_reference = SecretRef::secret_service(ProfileId::new().as_uuid());
    let private_key = fs::read(&client_key)?;
    let secrets = MultipleSecrets(vec![
        (key_reference.clone(), private_key.clone()),
        (passphrase_reference.clone(), passphrase.to_vec()),
    ]);
    let trust = HostTrust {
        known_hosts_file: known_hosts.clone(),
        revoked_host_keys_file: None,
        policy: HostKeyPolicy::Strict,
    };
    let mut terminal_profile = encrypted_key_profile(
        RemoteProtocol::Ssh,
        terminal_port,
        &username,
        &key_reference,
        &passphrase_reference,
        ProfileOptions::Ssh {
            jump_profiles: Vec::new(),
            agent_forwarding: false,
        },
    )?;
    terminal_profile.trust = TrustPolicy::SshKnownHosts {
        first_use: FirstUsePolicy::AskUser,
    };
    let terminal_adapter = SshTerminalAdapter::new(trust.clone())?;
    let mut terminal = terminal_adapter
        .open(&terminal_profile, &secrets, PtySize::new(24, 80)?, true)
        .await?;
    assert_process_does_not_expose(terminal.process_id(), &[passphrase, private_key.as_slice()])?;
    verify_terminal_marker(&mut terminal)?;

    let wrong_secrets = MultipleSecrets(vec![
        (key_reference.clone(), private_key.clone()),
        (passphrase_reference.clone(), b"wrong-passphrase".to_vec()),
    ]);
    terminal_profile.trust = TrustPolicy::SshKnownHosts {
        first_use: FirstUsePolicy::Reject,
    };
    let mut rejected = terminal_adapter
        .open(
            &terminal_profile,
            &wrong_secrets,
            PtySize::new(24, 80)?,
            false,
        )
        .await?;
    wait_for_terminal_disconnect(&mut rejected, DisconnectReason::AuthenticationFailed)?;

    let sftp_profile = encrypted_key_profile(
        RemoteProtocol::Sftp,
        sftp_port,
        &username,
        &key_reference,
        &passphrase_reference,
        ProfileOptions::Sftp {
            jump_profiles: Vec::new(),
        },
    )?;
    let sftp_adapter = SftpRemoteFileAdapter::new(trust)?;
    // connect() completes the encrypted-key handshake and a structured
    // metadata(".") request before returning the session.
    let session = sftp_adapter.connect(&sftp_profile, &secrets).await?;
    session.disconnect().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an explicitly provisioned isolated password SSH endpoint"]
async fn password_terminal_and_sftp_use_sealed_askpass_without_secret_leaks()
-> Result<(), Box<dyn Error>> {
    let host = env::var("LOCALDESK_SSH_PASSWORD_LIVE_HOST")?;
    let port = env::var("LOCALDESK_SSH_PASSWORD_LIVE_PORT")?.parse::<u16>()?;
    let username = env::var("LOCALDESK_SSH_PASSWORD_LIVE_USER")?;
    let known_hosts = env::var("LOCALDESK_SSH_PASSWORD_LIVE_KNOWN_HOSTS")?;
    let mut password = Vec::new();
    std::io::stdin()
        .take(8 * 1024 + 1)
        .read_to_end(&mut password)?;
    if password.is_empty()
        || password.len() > 8 * 1024
        || password.contains(&0)
        || password.contains(&b'\n')
        || password.contains(&b'\r')
    {
        return Err("live password stdin is empty or invalid".into());
    }

    let reference = SecretRef::secret_service(ProfileId::new().as_uuid());
    let secrets = SingleSecret {
        reference: reference.clone(),
        value: password.clone(),
    };
    let trust = HostTrust {
        known_hosts_file: known_hosts.into(),
        revoked_host_keys_file: None,
        policy: HostKeyPolicy::Strict,
    };
    let terminal_profile = password_profile(
        RemoteProtocol::Ssh,
        &host,
        port,
        &username,
        &reference,
        ProfileOptions::Ssh {
            jump_profiles: Vec::new(),
            agent_forwarding: false,
        },
    )?;
    let terminal_adapter = SshTerminalAdapter::new(trust.clone())?;
    let mut terminal = terminal_adapter
        .open(&terminal_profile, &secrets, PtySize::new(24, 80)?, false)
        .await?;
    assert_process_does_not_expose(terminal.process_id(), &[password.as_slice()])?;
    verify_terminal_marker(&mut terminal)?;

    let wrong_secrets = SingleSecret {
        reference: reference.clone(),
        value: b"definitely-wrong-localdesk-password".to_vec(),
    };
    let mut rejected = terminal_adapter
        .open(
            &terminal_profile,
            &wrong_secrets,
            PtySize::new(24, 80)?,
            false,
        )
        .await?;
    wait_for_terminal_disconnect(&mut rejected, DisconnectReason::AuthenticationFailed)?;

    let sftp_profile = password_profile(
        RemoteProtocol::Sftp,
        &host,
        port,
        &username,
        &reference,
        ProfileOptions::Sftp {
            jump_profiles: Vec::new(),
        },
    )?;
    let sftp_adapter = SftpRemoteFileAdapter::new(trust)?;
    let session = sftp_adapter.connect(&sftp_profile, &secrets).await?;
    session.disconnect().await?;
    password.fill(0);
    Ok(())
}

async fn verify_terminal(
    port: u16,
    username: &str,
    reference: &SecretRef,
    secrets: &SingleSecret,
    trust: HostTrust,
) -> Result<(), Box<dyn Error>> {
    let profile = profile(
        RemoteProtocol::Ssh,
        port,
        username,
        reference,
        ProfileOptions::Ssh {
            jump_profiles: Vec::new(),
            agent_forwarding: false,
        },
    )?;
    let adapter = SshTerminalAdapter::new(trust)?;
    let mut terminal = adapter
        .open(&profile, secrets, PtySize::new(24, 80)?, false)
        .await?;
    verify_terminal_marker(&mut terminal)
}

async fn verify_first_use_confirmation(
    port: u16,
    username: &str,
    reference: &SecretRef,
    secrets: &SingleSecret,
    trust: HostTrust,
    known_hosts: &Path,
) -> Result<(), Box<dyn Error>> {
    let mut profile = profile(
        RemoteProtocol::Ssh,
        port,
        username,
        reference,
        ProfileOptions::Ssh {
            jump_profiles: Vec::new(),
            agent_forwarding: false,
        },
    )?;
    profile.trust = TrustPolicy::SshKnownHosts {
        first_use: FirstUsePolicy::AskUser,
    };
    let adapter = SshTerminalAdapter::new(trust)?;
    let mut rejected = adapter
        .open(&profile, secrets, PtySize::new(24, 80)?, false)
        .await?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = rejected.poll_state()?;
        if status.state
            == (SessionState::Disconnected {
                reason: DisconnectReason::HostKeyUnknown,
            })
        {
            break;
        }
        if status.state != SessionState::Running || Instant::now() >= deadline {
            return Err(format!("expected host_key_unknown, got {:?}", status.state).into());
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(fs::read(known_hosts)?.is_empty());

    let mut accepted = adapter
        .open(&profile, secrets, PtySize::new(24, 80)?, true)
        .await?;
    verify_terminal_marker(&mut accepted)?;
    let recorded = fs::read_to_string(known_hosts)?;
    assert!(!recorded.trim().is_empty());
    let lookup = Command::new(SSH_KEYGEN_PROGRAM)
        .args(["-F", &format!("[127.0.0.1]:{port}"), "-f"])
        .arg(known_hosts)
        .output()?;
    assert!(
        lookup.status.success(),
        "accepted host key was not queryable: {}",
        String::from_utf8_lossy(&lookup.stderr)
    );
    Ok(())
}

fn verify_terminal_marker(
    terminal: &mut localdesk_remote_ssh::SshTerminalSession,
) -> Result<(), Box<dyn Error>> {
    terminal.write_input(
        b"printf '\\154\\157\\143\\141\\154\\144\\145\\163\\153\\055\\164\\145\\162\\155\\151\\156\\141\\154\\055\\154\\151\\166\\145\\055\\157\\153\\nlocaldesk-terminal-type=%s\\n' \"$TERM\"; exit\n",
    )?;

    let deadline = Instant::now() + Duration::from_secs(8);
    let mut transcript = Vec::new();
    while Instant::now() < deadline {
        let status = terminal.poll_state()?;
        match terminal.read_output(4 * 1024)? {
            TerminalRead::Data(output) => transcript.extend_from_slice(output.as_bytes()),
            TerminalRead::Pending => thread::sleep(Duration::from_millis(20)),
            TerminalRead::EndOfStream => break,
        }
        let has_marker = transcript
            .windows(b"localdesk-terminal-live-ok".len())
            .any(|window| window == b"localdesk-terminal-live-ok");
        let has_terminal_type = transcript
            .windows(b"localdesk-terminal-type=xterm-256color".len())
            .any(|window| window == b"localdesk-terminal-type=xterm-256color");
        if has_marker && has_terminal_type {
            let _ = terminal.close()?;
            return Ok(());
        }
        if status.state != SessionState::Running {
            break;
        }
    }
    Err(format!(
        "terminal marker missing; output={}",
        String::from_utf8_lossy(&transcript)
    )
    .into())
}

fn wait_for_terminal_disconnect(
    terminal: &mut localdesk_remote_ssh::SshTerminalSession,
    expected: DisconnectReason,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        let status = terminal.poll_state()?;
        if matches!(
            &status.state,
            SessionState::Disconnected { reason } if reason == &expected
        ) {
            return Ok(());
        }
        if status.state != SessionState::Running {
            return Err(format!("expected {expected:?}, got {:?}", status.state).into());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Err(format!("terminal did not report {expected:?} before deadline").into())
}

async fn verify_sftp(
    workspace: &Path,
    port: u16,
    username: &str,
    reference: &SecretRef,
    secrets: &SingleSecret,
    trust: HostTrust,
) -> Result<(), Box<dyn Error>> {
    let remote_root = workspace.join("remote-root");
    fs::create_dir(&remote_root)?;
    fs::write(remote_root.join("source.txt"), b"loopback-source")?;

    let profile = profile(
        RemoteProtocol::Sftp,
        port,
        username,
        reference,
        ProfileOptions::Sftp {
            jump_profiles: Vec::new(),
        },
    )?;
    let adapter = SftpRemoteFileAdapter::new(trust.clone())?;
    let session = adapter.connect(&profile, secrets).await?;
    let root = remote_path(&remote_root)?;
    let entries = session.list(&root).await?;
    assert!(
        entries.iter().any(|entry| entry.name == "source.txt"),
        "loopback listing did not contain source.txt: {entries:#?}"
    );

    let source = remote_path(&remote_root.join("source.txt"))?;
    let chunk = session
        .read_chunk(RemoteReadRequest {
            path: source,
            offset: 0,
            max_bytes: 1024,
            expected_identity: None,
        })
        .await?;
    assert_eq!(chunk.bytes, b"loopback-source");
    assert!(chunk.eof);

    let final_path = remote_path(&remote_root.join("uploaded.txt"))?;
    let temporary_path = remote_path(&remote_root.join("uploaded.txt.part"))?;
    let payload = b"loopback-upload".to_vec();
    let started = session
        .begin_write(BeginWriteRequest {
            final_path: final_path.clone(),
            temporary_path,
            expected_size_bytes: Some(payload.len() as u64),
            resume_from: None,
            expected_destination: None,
        })
        .await
        .map_err(|error| std::io::Error::other(format!("begin_write: {error:?}")))?;
    let written = session
        .write_chunk(started.handle, 0, payload.clone())
        .await
        .map_err(|error| std::io::Error::other(format!("write_chunk: {error:?}")))?;
    assert_eq!(written.next_offset, payload.len() as u64);
    let committed = session
        .commit_write(started.handle, None)
        .await
        .map_err(|error| std::io::Error::other(format!("commit_write: {error:?}")))?;
    assert_eq!(committed.path, final_path);

    let renamed = remote_path(&remote_root.join("renamed.txt"))?;
    if let Err(error) = session.rename(&final_path, &renamed).await {
        let source_exists = remote_root.join("uploaded.txt").exists();
        let destination_exists = remote_root.join("renamed.txt").exists();
        let diagnostic =
            diagnose_batch_rename(workspace, port, username, &trust, &final_path, &renamed)?;
        return Err(std::io::Error::other(format!(
            "rename: {error:?}; source_exists={source_exists}; destination_exists={destination_exists}; {diagnostic}"
        ))
        .into());
    }
    assert_eq!(fs::read(remote_root.join("renamed.txt"))?, payload);
    session.delete(&renamed).await?;
    assert!(!remote_root.join("renamed.txt").exists());
    session.disconnect().await?;
    Ok(())
}

fn diagnose_batch_rename(
    workspace: &Path,
    port: u16,
    username: &str,
    trust: &HostTrust,
    source: &RemotePath,
    destination: &RemotePath,
) -> Result<String, Box<dyn Error>> {
    if Path::new(destination.as_str()).exists() && !Path::new(source.as_str()).exists() {
        fs::rename(destination.as_str(), source.as_str())?;
    }
    let profile = SshProfile {
        target: PrivateEndpoint {
            host: "127.0.0.1".to_owned(),
            port,
            user: Some(username.to_owned()),
            trust: trust.clone(),
            authentication: PrivateAuthentication::IdentityFile(workspace.join("client_key")),
        },
        jump_hosts: Vec::new(),
    };
    let output = OpenSshAdapter
        .start_sftp(
            &profile,
            &[
                SftpOperation::Rename {
                    from: source.as_str().to_owned(),
                    to: destination.as_str().to_owned(),
                },
                SftpOperation::Stat {
                    remote_path: destination.as_str().to_owned(),
                },
            ],
        )?
        .wait_with_output()?;
    Ok(format!(
        "direct_status={:?}; direct_stdout={:?}; direct_stderr={:?}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}

fn profile(
    protocol: RemoteProtocol,
    port: u16,
    username: &str,
    reference: &SecretRef,
    options: ProfileOptions,
) -> Result<RemoteConnectionProfile, Box<dyn Error>> {
    Ok(RemoteConnectionProfile::new(
        ProfileId::new(),
        format!("loopback-{protocol:?}"),
        protocol,
        RemoteEndpoint::new("127.0.0.1", port)?,
        Some(username.to_owned()),
        None,
        Authentication::SshKey {
            private_key: reference.clone(),
            passphrase: None,
        },
        TrustPolicy::SshKnownHosts {
            first_use: FirstUsePolicy::Reject,
        },
        options,
    )?)
}

fn encrypted_key_profile(
    protocol: RemoteProtocol,
    port: u16,
    username: &str,
    private_key: &SecretRef,
    passphrase: &SecretRef,
    options: ProfileOptions,
) -> Result<RemoteConnectionProfile, Box<dyn Error>> {
    Ok(RemoteConnectionProfile::new(
        ProfileId::new(),
        format!("loopback-encrypted-{protocol:?}"),
        protocol,
        RemoteEndpoint::new("127.0.0.1", port)?,
        Some(username.to_owned()),
        None,
        Authentication::SshKey {
            private_key: private_key.clone(),
            passphrase: Some(passphrase.clone()),
        },
        TrustPolicy::SshKnownHosts {
            first_use: FirstUsePolicy::Reject,
        },
        options,
    )?)
}

fn password_profile(
    protocol: RemoteProtocol,
    host: &str,
    port: u16,
    username: &str,
    password: &SecretRef,
    options: ProfileOptions,
) -> Result<RemoteConnectionProfile, Box<dyn Error>> {
    Ok(RemoteConnectionProfile::new(
        ProfileId::new(),
        format!("password-live-{protocol:?}"),
        protocol,
        RemoteEndpoint::new(host, port)?,
        Some(username.to_owned()),
        None,
        Authentication::Password {
            secret: password.clone(),
        },
        TrustPolicy::SshKnownHosts {
            first_use: FirstUsePolicy::Reject,
        },
        options,
    )?)
}

fn generate_key(path: &Path) -> Result<(), Box<dyn Error>> {
    let output = Command::new(SSH_KEYGEN_PROGRAM)
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(path)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "ssh-keygen failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(())
}

fn generate_key_with_passphrase(path: &Path, passphrase: &[u8]) -> Result<(), Box<dyn Error>> {
    let passphrase = std::str::from_utf8(passphrase)?;
    let output = Command::new(SSH_KEYGEN_PROGRAM)
        .args(["-q", "-t", "ed25519", "-N", passphrase, "-f"])
        .arg(path)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "ssh-keygen failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(())
}

fn append_known_host(
    known_hosts: &Path,
    host_public_key: &Path,
    port: u16,
) -> Result<(), Box<dyn Error>> {
    let public_key = fs::read_to_string(host_public_key)?;
    let mut fields = public_key.split_whitespace();
    let algorithm = fields.next().ok_or("host public key algorithm missing")?;
    let encoded = fields.next().ok_or("host public key body missing")?;
    use std::io::Write as _;
    writeln!(
        fs::OpenOptions::new().append(true).open(known_hosts)?,
        "[127.0.0.1]:{port} {algorithm} {encoded}"
    )?;
    Ok(())
}

fn assert_process_does_not_expose(pid: u32, secrets: &[&[u8]]) -> Result<(), Box<dyn Error>> {
    let cmdline = fs::read(format!("/proc/{pid}/cmdline"))?;
    let environ = fs::read(format!("/proc/{pid}/environ"))?;
    for secret in secrets {
        assert!(
            !contains_bytes(&cmdline, secret),
            "secret leaked through argv"
        );
        assert!(
            !contains_bytes(&environ, secret),
            "secret leaked through environment"
        );
    }
    Ok(())
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn start_sshd(
    workspace: &Path,
    host_key: &Path,
    authorized_keys: &Path,
    log_path: &Path,
    port: u16,
    force_command: Option<&str>,
) -> Result<SshdGuard, Box<dyn Error>> {
    let log = File::create(log_path)?;
    let mut command = Command::new(SSHD_PROGRAM);
    command
        .args(["-D", "-e", "-f", "/dev/null", "-h"])
        .arg(host_key)
        .arg("-p")
        .arg(port.to_string())
        .arg("-o")
        .arg("ListenAddress=127.0.0.1")
        .arg("-o")
        .arg(format!("AuthorizedKeysFile={}", authorized_keys.display()))
        .args([
            "-o",
            "StrictModes=no",
            "-o",
            "UsePAM=no",
            "-o",
            "PasswordAuthentication=no",
            "-o",
            "KbdInteractiveAuthentication=no",
            "-o",
            "PubkeyAuthentication=yes",
            "-o",
            "AuthenticationMethods=publickey",
            "-o",
            "Subsystem=sftp internal-sftp",
        ]);
    if let Some(forced_command) = force_command {
        command
            .arg("-o")
            .arg(format!("ForceCommand={forced_command}"));
    }
    let mut child = command
        .arg("-o")
        .arg(format!(
            "PidFile={}",
            workspace.join(format!("sshd-{port}.pid")).display()
        ))
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .spawn()?;

    let deadline = Instant::now() + Duration::from_secs(5);
    let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            return Err(format!(
                "loopback sshd exited early with {status}: {}",
                read_log(log_path)
            )
            .into());
        }
        if TcpStream::connect_timeout(&address.into(), Duration::from_millis(50)).is_ok() {
            return Ok(SshdGuard { child });
        }
        thread::sleep(Duration::from_millis(25));
    }
    let _ = child.kill();
    let _ = child.wait();
    Err(format!("loopback sshd did not listen: {}", read_log(log_path)).into())
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

fn remote_path(path: &Path) -> Result<RemotePath, Box<dyn Error>> {
    Ok(RemotePath::new(
        path.to_str().ok_or("temporary path is not UTF-8")?,
    )?)
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
