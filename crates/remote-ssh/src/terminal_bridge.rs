use crate::{
    AdapterError, HostTrust, JumpProfileResolver, OpenSshAdapter, ProfileError, PtyError, PtySize,
    TERMINAL_CAPABILITIES, TerminalCapabilities, TerminalError, TerminalRead, TerminalSession,
    TerminalStatus,
    bridge::{prepare_terminal_profile, validate_bridge_trust},
};
use localdesk_remote_core::{
    AdapterAvailability, AdapterFuture, RemoteConnectionProfile, RemoteError, RemoteErrorKind,
    RemoteOperation, RemoteProtocol, RetryDisposition, SafeReason, SecretStore,
};
use std::{fmt, os::unix::fs::PermissionsExt, sync::Arc};
use tempfile::NamedTempFile;

const SSH_PROGRAM: &str = "/usr/bin/ssh";

#[derive(Debug, Default)]
struct NoTerminalJumpProfileResolver;

impl JumpProfileResolver for NoTerminalJumpProfileResolver {
    fn resolve<'a>(
        &'a self,
        _profile_id: localdesk_remote_core::ProfileId,
    ) -> AdapterFuture<'a, Result<RemoteConnectionProfile, RemoteError>> {
        Box::pin(async {
            Err(remote_error(
                RemoteErrorKind::Unsupported,
                RemoteOperation::Connect,
                "ssh_jump_profile_resolver_unavailable",
                RetryDisposition::UserAction,
            ))
        })
    }
}

pub struct SshTerminalAdapter {
    trust: HostTrust,
    jump_profiles: Arc<dyn JumpProfileResolver>,
    availability: AdapterAvailability,
}

impl SshTerminalAdapter {
    pub fn new(trust: HostTrust) -> Result<Self, ProfileError> {
        Self::with_jump_profile_resolver(trust, Arc::new(NoTerminalJumpProfileResolver))
    }

    pub fn with_jump_profile_resolver(
        trust: HostTrust,
        jump_profiles: Arc<dyn JumpProfileResolver>,
    ) -> Result<Self, ProfileError> {
        validate_bridge_trust(&trust)?;
        let availability = if executable(SSH_PROGRAM) {
            AdapterAvailability::Healthy
        } else {
            AdapterAvailability::Unsupported(reason("openssh_ssh_not_installed"))
        };
        Ok(Self {
            trust,
            jump_profiles,
            availability,
        })
    }

    pub const fn protocol(&self) -> RemoteProtocol {
        RemoteProtocol::Ssh
    }

    pub fn availability(&self) -> AdapterAvailability {
        self.availability.clone()
    }

    pub const fn capabilities(&self) -> TerminalCapabilities {
        TERMINAL_CAPABILITIES
    }

    pub fn open<'a>(
        &'a self,
        profile: &'a RemoteConnectionProfile,
        secrets: &'a dyn SecretStore,
        size: PtySize,
        accept_new_host_key: bool,
    ) -> AdapterFuture<'a, Result<SshTerminalSession, RemoteError>> {
        Box::pin(async move {
            if let AdapterAvailability::Unsupported(reason) = &self.availability {
                return Err(RemoteError::new(
                    RemoteErrorKind::Unsupported,
                    RemoteOperation::Connect,
                    reason.clone(),
                    RetryDisposition::UserAction,
                ));
            }
            let prepared = prepare_terminal_profile(
                &self.trust,
                self.jump_profiles.as_ref(),
                profile,
                secrets,
                accept_new_host_key,
            )
            .await?;
            let inner = OpenSshAdapter
                .open_terminal(&prepared.profile, size, prepared.askpass.as_ref())
                .map_err(map_open_error)?;
            Ok(SshTerminalSession {
                inner,
                identity_files: prepared.identity_files,
                askpass: prepared.askpass,
            })
        })
    }
}

impl fmt::Debug for SshTerminalAdapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SshTerminalAdapter")
            .field("trust", &self.trust)
            .field("jump_profiles", &"<resolver>")
            .field("availability", &self.availability)
            .finish()
    }
}

pub struct SshTerminalSession {
    // Field order is intentional: the child is closed/reaped before identity files are removed.
    inner: TerminalSession,
    identity_files: Vec<NamedTempFile>,
    askpass: Option<crate::askpass::AskpassSecret>,
}

impl SshTerminalSession {
    pub fn process_id(&self) -> u32 {
        self.inner.process_id()
    }

    pub const fn capabilities(&self) -> TerminalCapabilities {
        TERMINAL_CAPABILITIES
    }

    pub fn read_output(&mut self, max_bytes: usize) -> Result<TerminalRead, RemoteError> {
        self.inner
            .read_output(max_bytes)
            .map_err(|error| map_terminal_error(error, RemoteOperation::Read))
    }

    pub fn write_input(&mut self, input: &[u8]) -> Result<(), RemoteError> {
        self.inner
            .write_input(input)
            .map_err(|error| map_terminal_error(error, RemoteOperation::Write))
    }

    pub fn resize(&self, size: PtySize) -> Result<(), RemoteError> {
        self.inner
            .resize(size)
            .map_err(|error| map_terminal_error(error, RemoteOperation::Connect))
    }

    pub fn poll_state(&mut self) -> Result<TerminalStatus, RemoteError> {
        self.inner
            .poll_state()
            .map_err(|error| map_terminal_error(error, RemoteOperation::Connect))
    }

    pub fn close(&mut self) -> Result<TerminalStatus, RemoteError> {
        self.inner
            .close()
            .map_err(|error| map_terminal_error(error, RemoteOperation::Disconnect))
    }
}

impl fmt::Debug for SshTerminalSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SshTerminalSession")
            .field("inner", &self.inner)
            .field(
                "identity_files",
                &format_args!("<redacted:{}>", self.identity_files.len()),
            )
            .field("askpass", &self.askpass)
            .finish()
    }
}

fn map_open_error(error: AdapterError) -> RemoteError {
    match error {
        AdapterError::InvalidProfile(_) => remote_error(
            RemoteErrorKind::InvalidInput,
            RemoteOperation::Connect,
            "ssh_adapter_profile_invalid",
            RetryDisposition::Never,
        ),
        AdapterError::Pty(PtyError::InvalidSize { .. }) => remote_error(
            RemoteErrorKind::InvalidInput,
            RemoteOperation::Connect,
            "ssh_terminal_size_invalid",
            RetryDisposition::Never,
        ),
        AdapterError::Pty(_) => remote_error(
            RemoteErrorKind::Transport,
            RemoteOperation::Connect,
            "ssh_terminal_spawn_failed",
            RetryDisposition::Backoff,
        ),
        AdapterError::EmptySftpBatch
        | AdapterError::InvalidSftpPath { .. }
        | AdapterError::InvalidLocalPath { .. }
        | AdapterError::CreateInput(_)
        | AdapterError::WriteInput(_)
        | AdapterError::SpawnSftp(_)
        | AdapterError::InspectSftp(_)
        | AdapterError::CloseSftp(_)
        | AdapterError::StructuredSftpHandshake { .. }
        | AdapterError::StructuredSftp(_)
        | AdapterError::SftpOutputLimit { .. } => remote_error(
            RemoteErrorKind::RemoteProtocol,
            RemoteOperation::Connect,
            "ssh_terminal_adapter_unexpected_error",
            RetryDisposition::Never,
        ),
    }
}

fn map_terminal_error(error: TerminalError, operation: RemoteOperation) -> RemoteError {
    match error {
        TerminalError::InvalidReadLimit { .. } => remote_error(
            RemoteErrorKind::InvalidInput,
            operation,
            "ssh_terminal_read_limit_invalid",
            RetryDisposition::Never,
        ),
        TerminalError::InputTooLarge { .. } => remote_error(
            RemoteErrorKind::InvalidInput,
            operation,
            "ssh_terminal_input_too_large",
            RetryDisposition::Never,
        ),
        TerminalError::Pty(PtyError::InvalidSize { .. }) => remote_error(
            RemoteErrorKind::InvalidInput,
            operation,
            "ssh_terminal_size_invalid",
            RetryDisposition::Never,
        ),
        TerminalError::Read(_) => remote_error(
            RemoteErrorKind::Transport,
            operation,
            "ssh_terminal_read_failed",
            RetryDisposition::Backoff,
        ),
        TerminalError::Write(_) => remote_error(
            RemoteErrorKind::Transport,
            operation,
            "ssh_terminal_write_failed",
            RetryDisposition::Backoff,
        ),
        TerminalError::Pty(_) => remote_error(
            RemoteErrorKind::Transport,
            operation,
            "ssh_terminal_pty_failed",
            RetryDisposition::Backoff,
        ),
    }
}

fn executable(program: &str) -> bool {
    std::fs::metadata(program)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn reason(value: &'static str) -> SafeReason {
    SafeReason::new(value).expect("static safe reason")
}

fn remote_error(
    kind: RemoteErrorKind,
    operation: RemoteOperation,
    reason_value: &'static str,
    retry: RetryDisposition,
) -> RemoteError {
    RemoteError::new(kind, operation, reason(reason_value), retry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Authentication as PrivateAuthentication, HostKeyPolicy};
    use localdesk_remote_core::{
        AdapterFuture, Authentication, FirstUsePolicy, ProfileId, ProfileOptions, RemoteEndpoint,
        SecretRef, SecretStoreError, SecretValue, TrustPolicy,
    };
    use std::{
        collections::HashMap,
        fs,
        future::Future,
        pin::Pin,
        sync::atomic::{AtomicUsize, Ordering},
        task::{Context, Poll},
    };

    fn test_trust() -> HostTrust {
        HostTrust {
            known_hosts_file: "/tmp/localdesk-terminal-known-hosts".into(),
            revoked_host_keys_file: Some("/tmp/localdesk-terminal-revoked".into()),
            policy: HostKeyPolicy::Strict,
        }
    }

    fn profile(
        authentication: Authentication,
        jump_profiles: Vec<ProfileId>,
        agent_forwarding: bool,
    ) -> RemoteConnectionProfile {
        RemoteConnectionProfile::new(
            ProfileId::new(),
            "terminal fixture",
            RemoteProtocol::Ssh,
            RemoteEndpoint::new("terminal.example.test", 22).expect("endpoint"),
            Some("operator".to_owned()),
            None,
            authentication,
            TrustPolicy::SshKnownHosts {
                first_use: FirstUsePolicy::Reject,
            },
            ProfileOptions::Ssh {
                jump_profiles,
                agent_forwarding,
            },
        )
        .expect("profile")
    }

    struct ImmediateSecrets {
        value: Vec<u8>,
        resolves: AtomicUsize,
    }

    impl SecretStore for ImmediateSecrets {
        fn resolve<'a>(
            &'a self,
            _reference: &'a SecretRef,
        ) -> AdapterFuture<'a, Result<SecretValue, SecretStoreError>> {
            self.resolves.fetch_add(1, Ordering::SeqCst);
            let value = self.value.clone();
            Box::pin(async move { Ok(SecretValue::new(value)) })
        }

        fn delete<'a>(
            &'a self,
            _reference: &'a SecretRef,
        ) -> AdapterFuture<'a, Result<(), SecretStoreError>> {
            Box::pin(async { panic!("terminal bridge must not delete secrets") })
        }
    }

    struct Profiles(HashMap<ProfileId, RemoteConnectionProfile>);

    impl JumpProfileResolver for Profiles {
        fn resolve<'a>(
            &'a self,
            profile_id: ProfileId,
        ) -> AdapterFuture<'a, Result<RemoteConnectionProfile, RemoteError>> {
            let value = self.0.get(&profile_id).cloned();
            Box::pin(async move {
                value.ok_or_else(|| {
                    remote_error(
                        RemoteErrorKind::NotFound,
                        RemoteOperation::Connect,
                        "jump_profile_not_found",
                        RetryDisposition::UserAction,
                    )
                })
            })
        }
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        let mut context = Context::from_waker(std::task::Waker::noop());
        let mut future = Box::pin(future);
        loop {
            if let Poll::Ready(value) = Pin::new(&mut future).poll(&mut context) {
                return value;
            }
            std::thread::yield_now();
        }
    }

    #[test]
    fn adapter_reports_system_openssh_availability_and_terminal_contract() {
        let adapter = SshTerminalAdapter::new(test_trust()).expect("adapter");
        assert_eq!(adapter.protocol(), RemoteProtocol::Ssh);
        assert_eq!(adapter.capabilities(), TERMINAL_CAPABILITIES);
        if executable(SSH_PROGRAM) {
            assert_eq!(adapter.availability(), AdapterAvailability::Healthy);
        } else {
            assert!(matches!(
                adapter.availability(),
                AdapterAvailability::Unsupported(_)
            ));
        }
    }

    #[test]
    fn terminal_profile_reuses_per_hop_trust_and_owner_only_identity_files() {
        let key_text = b"terminal-private-key-must-not-leak";
        let secrets = ImmediateSecrets {
            value: key_text.to_vec(),
            resolves: AtomicUsize::new(0),
        };
        let jump = profile(Authentication::SshAgent, Vec::new(), false);
        let jump_id = jump.id;
        let profiles = Profiles(HashMap::from([(jump_id, jump)]));
        let target = profile(
            Authentication::SshKey {
                private_key: SecretRef::secret_service(ProfileId::new().as_uuid()),
                passphrase: None,
            },
            vec![jump_id],
            false,
        );
        let prepared = block_on(prepare_terminal_profile(
            &test_trust(),
            &profiles,
            &target,
            &secrets,
            false,
        ))
        .expect("prepare terminal profile");

        assert_eq!(secrets.resolves.load(Ordering::SeqCst), 1);
        assert_eq!(prepared.profile.jump_hosts.len(), 1);
        assert_eq!(prepared.identity_files.len(), 1);
        assert!(matches!(
            prepared.profile.target.authentication,
            PrivateAuthentication::IdentityFile(_)
        ));
        for endpoint in
            std::iter::once(&prepared.profile.target).chain(prepared.profile.jump_hosts.iter())
        {
            assert_eq!(endpoint.trust, test_trust());
        }
        let identity = &prepared.identity_files[0];
        assert_eq!(
            identity
                .as_file()
                .metadata()
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(fs::read(identity.path()).expect("identity"), key_text);
        let identity_path = identity.path().to_owned();
        assert!(!format!("{prepared:?}").contains("terminal-private-key"));
        drop(prepared);
        assert!(
            !identity_path.exists(),
            "identity must be removed with its lease"
        );
    }

    #[test]
    fn explicit_first_use_confirmation_accepts_only_the_target_new_key() {
        let secrets = ImmediateSecrets {
            value: b"unused".to_vec(),
            resolves: AtomicUsize::new(0),
        };
        let jump = profile(Authentication::SshAgent, Vec::new(), false);
        let jump_id = jump.id;
        let profiles = Profiles(HashMap::from([(jump_id, jump)]));
        let mut target = profile(Authentication::SshAgent, vec![jump_id], false);
        target.trust = TrustPolicy::SshKnownHosts {
            first_use: FirstUsePolicy::AskUser,
        };

        let prepared = block_on(prepare_terminal_profile(
            &test_trust(),
            &profiles,
            &target,
            &secrets,
            true,
        ))
        .expect("explicit confirmation prepares the target");

        assert_eq!(
            prepared.profile.target.trust.policy,
            HostKeyPolicy::AcceptNew
        );
        assert_eq!(
            prepared.profile.jump_hosts[0].trust.policy,
            HostKeyPolicy::Strict,
        );
    }

    #[test]
    fn reject_policy_cannot_be_overridden_by_a_confirmation_flag() {
        let secrets = ImmediateSecrets {
            value: b"unused".to_vec(),
            resolves: AtomicUsize::new(0),
        };
        let target = profile(Authentication::SshAgent, Vec::new(), false);
        let error = block_on(prepare_terminal_profile(
            &test_trust(),
            &Profiles(HashMap::new()),
            &target,
            &secrets,
            true,
        ))
        .expect_err("reject policy must not accept a new key");
        assert_eq!(error.kind, RemoteErrorKind::InvalidInput);
        assert_eq!(
            error.reason.as_str(),
            "ssh_first_use_confirmation_not_allowed"
        );
    }

    #[test]
    fn public_open_rejects_unsafe_target_options_before_spawning_ssh() {
        let adapter = SshTerminalAdapter::new(test_trust()).expect("adapter");
        let secrets = ImmediateSecrets {
            value: b"unused".to_vec(),
            resolves: AtomicUsize::new(0),
        };
        let forwarding = profile(Authentication::SshAgent, Vec::new(), true);
        let error = block_on(adapter.open(
            &forwarding,
            &secrets,
            PtySize::new(24, 80).expect("size"),
            false,
        ))
        .expect_err("agent forwarding must fail before spawn");
        assert_eq!(error.kind, RemoteErrorKind::Unsupported);
        assert_eq!(error.reason.as_str(), "ssh_agent_forwarding_forbidden");
        assert_eq!(secrets.resolves.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn terminal_bridge_rejects_non_ssh_profiles_before_spawning_ssh() {
        let adapter = SshTerminalAdapter::new(test_trust()).expect("adapter");
        let secrets = ImmediateSecrets {
            value: b"unused".to_vec(),
            resolves: AtomicUsize::new(0),
        };
        let sftp = RemoteConnectionProfile::new(
            ProfileId::new(),
            "sftp fixture",
            RemoteProtocol::Sftp,
            RemoteEndpoint::new("files.example.test", 22).expect("endpoint"),
            Some("operator".to_owned()),
            None,
            Authentication::SshAgent,
            TrustPolicy::SshKnownHosts {
                first_use: FirstUsePolicy::Reject,
            },
            ProfileOptions::Sftp {
                jump_profiles: Vec::new(),
            },
        )
        .expect("SFTP profile");
        let error =
            block_on(adapter.open(&sftp, &secrets, PtySize::new(24, 80).expect("size"), false))
                .expect_err("SFTP profile must fail before spawn");
        assert_eq!(error.kind, RemoteErrorKind::InvalidInput);
        assert_eq!(error.reason.as_str(), "ssh_profile_required");
        assert_eq!(secrets.resolves.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn ask_user_direct_secrets_and_nested_jump_follow_explicit_policy() {
        let secrets = ImmediateSecrets {
            value: b"unused".to_vec(),
            resolves: AtomicUsize::new(0),
        };
        let resolver = Profiles(HashMap::new());

        let mut ask = profile(Authentication::SshAgent, Vec::new(), false);
        ask.trust = TrustPolicy::SshKnownHosts {
            first_use: FirstUsePolicy::AskUser,
        };
        let prepared = block_on(prepare_terminal_profile(
            &test_trust(),
            &resolver,
            &ask,
            &secrets,
            false,
        ))
        .expect("AskUser must probe with strict host-key checking");
        assert_eq!(prepared.profile.target.trust.policy, HostKeyPolicy::Strict);

        let password = profile(
            Authentication::Password {
                secret: SecretRef::secret_service(ProfileId::new().as_uuid()),
            },
            Vec::new(),
            false,
        );
        let prepared = block_on(prepare_terminal_profile(
            &test_trust(),
            &resolver,
            &password,
            &secrets,
            false,
        ))
        .expect("direct password must be prepared through askpass");
        assert!(matches!(
            prepared.profile.target.authentication,
            PrivateAuthentication::Password
        ));
        assert!(prepared.askpass.is_some());
        assert_eq!(secrets.resolves.load(Ordering::SeqCst), 1);

        let passphrase = profile(
            Authentication::SshKey {
                private_key: SecretRef::secret_service(ProfileId::new().as_uuid()),
                passphrase: Some(SecretRef::secret_service(ProfileId::new().as_uuid())),
            },
            Vec::new(),
            false,
        );
        let prepared = block_on(prepare_terminal_profile(
            &test_trust(),
            &resolver,
            &passphrase,
            &secrets,
            false,
        ))
        .expect("direct encrypted key must be prepared through askpass");
        assert!(matches!(
            prepared.profile.target.authentication,
            PrivateAuthentication::IdentityFileWithPassphrase(_)
        ));
        assert_eq!(prepared.identity_files.len(), 1);
        assert!(prepared.askpass.is_some());
        assert_eq!(secrets.resolves.load(Ordering::SeqCst), 3);

        let nested_id = ProfileId::new();
        let jump = profile(Authentication::SshAgent, vec![nested_id], false);
        let jump_id = jump.id;
        let resolver = Profiles(HashMap::from([(jump_id, jump)]));
        let target = profile(Authentication::SshAgent, vec![jump_id], false);
        let error = block_on(prepare_terminal_profile(
            &test_trust(),
            &resolver,
            &target,
            &secrets,
            false,
        ))
        .expect_err("nested jump must fail");
        assert_eq!(
            error.reason.as_str(),
            "ssh_nested_jump_profiles_not_supported"
        );

        let forwarding_jump = profile(Authentication::SshAgent, Vec::new(), true);
        let forwarding_jump_id = forwarding_jump.id;
        let resolver = Profiles(HashMap::from([(forwarding_jump_id, forwarding_jump)]));
        let target = profile(Authentication::SshAgent, vec![forwarding_jump_id], false);
        let error = block_on(prepare_terminal_profile(
            &test_trust(),
            &resolver,
            &target,
            &secrets,
            false,
        ))
        .expect_err("jump agent forwarding must fail");
        assert_eq!(error.reason.as_str(), "ssh_agent_forwarding_forbidden");
        assert_eq!(secrets.resolves.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn adapter_and_terminal_errors_map_to_safe_typed_remote_errors() {
        let open = map_open_error(AdapterError::Pty(PtyError::InvalidSize {
            rows: 0,
            columns: 80,
            pixel_width: 0,
            pixel_height: 0,
        }));
        assert_eq!(open.kind, RemoteErrorKind::InvalidInput);
        assert_eq!(open.reason.as_str(), "ssh_terminal_size_invalid");

        let read = map_terminal_error(
            TerminalError::InvalidReadLimit {
                requested: 0,
                maximum: 64,
            },
            RemoteOperation::Read,
        );
        assert_eq!(read.kind, RemoteErrorKind::InvalidInput);
        assert_eq!(read.operation, RemoteOperation::Read);
        assert_eq!(read.reason.as_str(), "ssh_terminal_read_limit_invalid");
        assert!(!format!("{read:?}").contains("private"));
    }
}
