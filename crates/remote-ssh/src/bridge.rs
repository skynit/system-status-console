use crate::{
    AdapterError, Authentication as SshAuthentication, Endpoint, HostKeyPolicy, HostTrust,
    OpenSshAdapter, SftpOperation, SshProfile,
    askpass::AskpassSecret,
    openssh::{SftpOutput, StructuredSftpSession},
};
use bytes::BytesMut;
use futures_core::Stream;
use localdesk_remote_core::{
    AdapterAvailability, AdapterFuture, Authentication, BeginWriteRequest, CapabilityMatrix,
    CapabilityStatus, ConnectionState, EntryKind, FILE_OPERATIONS, FileOperation,
    MAX_REMOTE_CHUNK_BYTES, ObjectIdentity, OperationCapability, ProfileId, ProfileOptions,
    RemoteConnectionProfile, RemoteEntry, RemoteError, RemoteErrorKind, RemoteFileAdapter,
    RemoteFileSession, RemoteIoControl, RemoteIoControlSupport, RemoteOperation, RemotePath,
    RemoteProtocol, RemoteReadChunk, RemoteReadRequest, RemoteSession, RemoteWriteHandle,
    RemoteWriteReceipt, RetryDisposition, SafeReason, SecretStore, SecretStoreError, SessionId,
    TrustPolicy,
};
use openssh_sftp_client::{Error as StructuredSftpError, Sftp, error::SftpErrorKind};
use std::{
    collections::HashMap,
    fmt,
    future::Future,
    io::{SeekFrom, Write},
    os::unix::fs::PermissionsExt,
    path::Path,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tempfile::{Builder, NamedTempFile};
use tokio::{io::AsyncSeekExt, sync::Mutex as AsyncMutex};

const SSH_PROGRAM: &str = "/usr/bin/ssh";
const SFTP_PROGRAM: &str = "/usr/bin/sftp";

pub trait JumpProfileResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        profile_id: ProfileId,
    ) -> AdapterFuture<'a, Result<RemoteConnectionProfile, RemoteError>>;
}

#[derive(Debug, Default)]
pub struct NoJumpProfileResolver;

impl JumpProfileResolver for NoJumpProfileResolver {
    fn resolve<'a>(
        &'a self,
        _profile_id: ProfileId,
    ) -> AdapterFuture<'a, Result<RemoteConnectionProfile, RemoteError>> {
        Box::pin(async {
            Err(error(
                RemoteErrorKind::Unsupported,
                RemoteOperation::Connect,
                "sftp_jump_profile_resolver_unavailable",
                RetryDisposition::UserAction,
            ))
        })
    }
}

pub struct SftpRemoteFileAdapter {
    trust: HostTrust,
    jump_profiles: Arc<dyn JumpProfileResolver>,
    capabilities: CapabilityMatrix,
    availability: AdapterAvailability,
}

impl fmt::Debug for SftpRemoteFileAdapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SftpRemoteFileAdapter")
            .field("trust", &self.trust)
            .field("jump_profiles", &"<resolver>")
            .field("capabilities", &self.capabilities)
            .field("availability", &self.availability)
            .finish()
    }
}

impl SftpRemoteFileAdapter {
    pub fn new(trust: HostTrust) -> Result<Self, crate::ProfileError> {
        Self::with_jump_profile_resolver(trust, Arc::new(NoJumpProfileResolver))
    }

    pub fn with_jump_profile_resolver(
        trust: HostTrust,
        jump_profiles: Arc<dyn JumpProfileResolver>,
    ) -> Result<Self, crate::ProfileError> {
        validate_bridge_trust(&trust)?;
        let capabilities = sftp_capabilities();
        let availability = if executable(SSH_PROGRAM) && executable(SFTP_PROGRAM) {
            AdapterAvailability::Healthy
        } else {
            AdapterAvailability::Unsupported(reason("openssh_sftp_not_installed"))
        };
        Ok(Self {
            trust,
            jump_profiles,
            capabilities,
            availability,
        })
    }
}

impl RemoteFileAdapter for SftpRemoteFileAdapter {
    fn protocol(&self) -> RemoteProtocol {
        RemoteProtocol::Sftp
    }

    fn availability(&self) -> AdapterAvailability {
        self.availability.clone()
    }

    fn capabilities(&self) -> &CapabilityMatrix {
        &self.capabilities
    }

    fn io_control_support(&self) -> RemoteIoControlSupport {
        RemoteIoControlSupport::Supported
    }

    fn connect<'a>(
        &'a self,
        profile: &'a RemoteConnectionProfile,
        secrets: &'a dyn SecretStore,
    ) -> AdapterFuture<'a, Result<Box<dyn RemoteFileSession>, RemoteError>> {
        self.connect_controlled(profile, secrets, default_operation_control())
    }

    fn connect_controlled<'a>(
        &'a self,
        profile: &'a RemoteConnectionProfile,
        secrets: &'a dyn SecretStore,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<Box<dyn RemoteFileSession>, RemoteError>> {
        Box::pin(async move {
            if let AdapterAvailability::Unsupported(reason) = &self.availability {
                return Err(RemoteError::new(
                    RemoteErrorKind::Unsupported,
                    RemoteOperation::Connect,
                    reason.clone(),
                    RetryDisposition::UserAction,
                ));
            }
            control.check(RemoteOperation::Connect)?;
            let prepared = prepare_profile(self, profile, secrets).await?;
            let structured = start_structured_sftp_controlled(
                &prepared.profile,
                prepared.askpass.as_ref(),
                RemoteOperation::Connect,
                control.clone(),
            )
            .await?;
            let supports_atomic_rename = match structured_sftp_operation(
                &structured,
                RemoteOperation::Connect,
                control,
                |sftp| {
                    Box::pin(async move {
                        let mut fs = sftp.fs();
                        fs.metadata(Path::new(".")).await?;
                        Ok(sftp.support_posix_rename())
                    })
                },
            )
            .await
            {
                Ok(value) => value,
                Err(error) => {
                    let _ = structured.close().await;
                    return Err(error);
                }
            };
            let capabilities = sftp_session_capabilities(supports_atomic_rename);
            let now = unix_time_ms();
            let session = RemoteSession {
                id: SessionId::new(),
                profile_id: profile.id,
                protocol: RemoteProtocol::Sftp,
                state: ConnectionState::Ready,
                capabilities: capabilities.clone(),
                opened_at_unix_ms: now,
                updated_at_unix_ms: now,
            };
            Ok(Box::new(SftpRemoteFileSession {
                inner: Arc::new(SessionInner {
                    profile: prepared.profile,
                    _identity_files: prepared.identity_files,
                    askpass: prepared.askpass,
                    capabilities,
                    session: Mutex::new(session),
                    writes: Mutex::new(HashMap::new()),
                    io_lock: AsyncMutex::new(()),
                    structured: AsyncMutex::new(Some(structured)),
                }),
            }) as Box<dyn RemoteFileSession>)
        })
    }
}

pub(crate) struct PreparedProfile {
    pub(crate) profile: SshProfile,
    pub(crate) identity_files: Vec<NamedTempFile>,
    pub(crate) askpass: Option<AskpassSecret>,
}

impl fmt::Debug for PreparedProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedProfile")
            .field("profile", &self.profile)
            .field(
                "identity_files",
                &format_args!("<redacted:{}>", self.identity_files.len()),
            )
            .field("askpass", &self.askpass)
            .finish()
    }
}

async fn prepare_profile(
    adapter: &SftpRemoteFileAdapter,
    profile: &RemoteConnectionProfile,
    secrets: &dyn SecretStore,
) -> Result<PreparedProfile, RemoteError> {
    profile.validate().map_err(|_| {
        error(
            RemoteErrorKind::InvalidInput,
            RemoteOperation::Connect,
            "sftp_profile_invalid",
            RetryDisposition::Never,
        )
    })?;
    if profile.protocol != RemoteProtocol::Sftp {
        return Err(error(
            RemoteErrorKind::InvalidInput,
            RemoteOperation::Connect,
            "sftp_profile_required",
            RetryDisposition::Never,
        ));
    }
    if profile.domain.is_some() {
        return Err(error(
            RemoteErrorKind::Unsupported,
            RemoteOperation::Connect,
            "sftp_domain_not_supported",
            RetryDisposition::UserAction,
        ));
    }
    let jump_ids = match &profile.options {
        ProfileOptions::Sftp { jump_profiles } => jump_profiles,
        _ => {
            return Err(error(
                RemoteErrorKind::InvalidInput,
                RemoteOperation::Connect,
                "sftp_options_required",
                RetryDisposition::Never,
            ));
        }
    };
    if jump_ids.len() > crate::profile::MAX_JUMP_HOSTS {
        return Err(error(
            RemoteErrorKind::InvalidInput,
            RemoteOperation::Connect,
            "sftp_jump_chain_too_long",
            RetryDisposition::Never,
        ));
    }

    let mut identity_files = Vec::new();
    let mut askpass = None;
    if !jump_ids.is_empty()
        && matches!(
            profile.authentication,
            Authentication::Password { .. }
                | Authentication::SshKey {
                    passphrase: Some(_),
                    ..
                }
        )
    {
        return Err(error(
            RemoteErrorKind::Unsupported,
            RemoteOperation::Connect,
            "sftp_jump_secret_routing_not_supported",
            RetryDisposition::UserAction,
        ));
    }
    let target = map_endpoint(
        profile,
        &adapter.trust,
        secrets,
        &mut identity_files,
        &mut askpass,
        BridgeKind::Sftp,
        false,
    )
    .await?;
    let mut jump_hosts = Vec::with_capacity(jump_ids.len());
    for profile_id in jump_ids {
        let jump = adapter.jump_profiles.resolve(*profile_id).await?;
        validate_jump_profile(&jump, BridgeKind::Sftp)?;
        jump_hosts.push(
            map_endpoint(
                &jump,
                &adapter.trust,
                secrets,
                &mut identity_files,
                &mut askpass,
                BridgeKind::Sftp,
                false,
            )
            .await?,
        );
    }
    let profile = SshProfile { target, jump_hosts };
    profile.validate().map_err(map_private_profile_error)?;
    Ok(PreparedProfile {
        profile,
        identity_files,
        askpass,
    })
}

#[derive(Clone, Copy)]
enum BridgeKind {
    Sftp,
    SshTerminal,
}

impl BridgeKind {
    fn jump_profile_invalid(self) -> &'static str {
        match self {
            Self::Sftp => "sftp_jump_profile_invalid",
            Self::SshTerminal => "ssh_jump_profile_invalid",
        }
    }

    fn jump_requires_ssh_profile(self) -> &'static str {
        match self {
            Self::Sftp => "sftp_jump_requires_ssh_profile",
            Self::SshTerminal => "ssh_jump_requires_ssh_profile",
        }
    }

    fn jump_domain_not_supported(self) -> &'static str {
        match self {
            Self::Sftp => "sftp_jump_domain_not_supported",
            Self::SshTerminal => "ssh_jump_domain_not_supported",
        }
    }

    fn nested_jump_profiles_not_supported(self) -> &'static str {
        match self {
            Self::Sftp => "sftp_nested_jump_profiles_not_supported",
            Self::SshTerminal => "ssh_nested_jump_profiles_not_supported",
        }
    }

    fn jump_requires_ssh_options(self) -> &'static str {
        match self {
            Self::Sftp => "sftp_jump_requires_ssh_options",
            Self::SshTerminal => "ssh_jump_requires_ssh_options",
        }
    }

    fn known_hosts_policy_required(self) -> &'static str {
        match self {
            Self::Sftp => "sftp_known_hosts_policy_required",
            Self::SshTerminal => "ssh_known_hosts_policy_required",
        }
    }

    fn first_use_confirmation_unavailable(self) -> &'static str {
        match self {
            Self::Sftp => "sftp_first_use_confirmation_not_implemented",
            Self::SshTerminal => "ssh_first_use_confirmation_not_allowed",
        }
    }

    fn authentication_invalid(self) -> &'static str {
        match self {
            Self::Sftp => "sftp_authentication_invalid",
            Self::SshTerminal => "ssh_authentication_invalid",
        }
    }
}

pub(crate) async fn prepare_terminal_profile(
    trust: &HostTrust,
    jump_profiles: &dyn JumpProfileResolver,
    profile: &RemoteConnectionProfile,
    secrets: &dyn SecretStore,
    accept_new_host_key: bool,
) -> Result<PreparedProfile, RemoteError> {
    profile.validate().map_err(|_| {
        error(
            RemoteErrorKind::InvalidInput,
            RemoteOperation::Connect,
            "ssh_profile_invalid",
            RetryDisposition::Never,
        )
    })?;
    if profile.protocol != RemoteProtocol::Ssh {
        return Err(error(
            RemoteErrorKind::InvalidInput,
            RemoteOperation::Connect,
            "ssh_profile_required",
            RetryDisposition::Never,
        ));
    }
    if profile.domain.is_some() {
        return Err(error(
            RemoteErrorKind::Unsupported,
            RemoteOperation::Connect,
            "ssh_domain_not_supported",
            RetryDisposition::UserAction,
        ));
    }
    let jump_ids = match &profile.options {
        ProfileOptions::Ssh {
            jump_profiles,
            agent_forwarding: false,
        } => jump_profiles,
        ProfileOptions::Ssh {
            agent_forwarding: true,
            ..
        } => {
            return Err(error(
                RemoteErrorKind::Unsupported,
                RemoteOperation::Connect,
                "ssh_agent_forwarding_forbidden",
                RetryDisposition::Never,
            ));
        }
        _ => {
            return Err(error(
                RemoteErrorKind::InvalidInput,
                RemoteOperation::Connect,
                "ssh_options_required",
                RetryDisposition::Never,
            ));
        }
    };
    if jump_ids.len() > crate::profile::MAX_JUMP_HOSTS {
        return Err(error(
            RemoteErrorKind::InvalidInput,
            RemoteOperation::Connect,
            "ssh_jump_chain_too_long",
            RetryDisposition::Never,
        ));
    }

    let mut identity_files = Vec::new();
    let mut askpass = None;
    if !jump_ids.is_empty()
        && matches!(
            profile.authentication,
            Authentication::Password { .. }
                | Authentication::SshKey {
                    passphrase: Some(_),
                    ..
                }
        )
    {
        return Err(error(
            RemoteErrorKind::Unsupported,
            RemoteOperation::Connect,
            "ssh_jump_secret_routing_not_supported",
            RetryDisposition::UserAction,
        ));
    }
    let target = map_endpoint(
        profile,
        trust,
        secrets,
        &mut identity_files,
        &mut askpass,
        BridgeKind::SshTerminal,
        accept_new_host_key,
    )
    .await?;
    let mut jump_hosts = Vec::with_capacity(jump_ids.len());
    for profile_id in jump_ids {
        let jump = jump_profiles.resolve(*profile_id).await?;
        validate_jump_profile(&jump, BridgeKind::SshTerminal)?;
        jump_hosts.push(
            map_endpoint(
                &jump,
                trust,
                secrets,
                &mut identity_files,
                &mut askpass,
                BridgeKind::SshTerminal,
                false,
            )
            .await?,
        );
    }
    let profile = SshProfile { target, jump_hosts };
    profile.validate().map_err(|_| {
        error(
            RemoteErrorKind::InvalidInput,
            RemoteOperation::Connect,
            "ssh_adapter_profile_invalid",
            RetryDisposition::Never,
        )
    })?;
    Ok(PreparedProfile {
        profile,
        identity_files,
        askpass,
    })
}

fn validate_jump_profile(
    profile: &RemoteConnectionProfile,
    bridge: BridgeKind,
) -> Result<(), RemoteError> {
    profile.validate().map_err(|_| {
        error(
            RemoteErrorKind::InvalidInput,
            RemoteOperation::Connect,
            bridge.jump_profile_invalid(),
            RetryDisposition::Never,
        )
    })?;
    if profile.protocol != RemoteProtocol::Ssh {
        return Err(error(
            RemoteErrorKind::InvalidInput,
            RemoteOperation::Connect,
            bridge.jump_requires_ssh_profile(),
            RetryDisposition::Never,
        ));
    }
    if profile.domain.is_some() {
        return Err(error(
            RemoteErrorKind::Unsupported,
            RemoteOperation::Connect,
            bridge.jump_domain_not_supported(),
            RetryDisposition::UserAction,
        ));
    }
    match &profile.options {
        ProfileOptions::Ssh {
            jump_profiles,
            agent_forwarding,
        } if jump_profiles.is_empty() && !agent_forwarding => Ok(()),
        ProfileOptions::Ssh {
            agent_forwarding: true,
            ..
        } => Err(error(
            RemoteErrorKind::Unsupported,
            RemoteOperation::Connect,
            "ssh_agent_forwarding_forbidden",
            RetryDisposition::Never,
        )),
        ProfileOptions::Ssh { .. } => Err(error(
            RemoteErrorKind::Unsupported,
            RemoteOperation::Connect,
            bridge.nested_jump_profiles_not_supported(),
            RetryDisposition::UserAction,
        )),
        _ => Err(error(
            RemoteErrorKind::InvalidInput,
            RemoteOperation::Connect,
            bridge.jump_requires_ssh_options(),
            RetryDisposition::Never,
        )),
    }
}

async fn map_endpoint(
    profile: &RemoteConnectionProfile,
    trust_files: &HostTrust,
    secrets: &dyn SecretStore,
    identity_files: &mut Vec<NamedTempFile>,
    askpass: &mut Option<AskpassSecret>,
    bridge: BridgeKind,
    accept_new_host_key: bool,
) -> Result<Endpoint, RemoteError> {
    let policy = match (&profile.trust, accept_new_host_key) {
        (
            TrustPolicy::SshKnownHosts {
                first_use: localdesk_remote_core::FirstUsePolicy::AskUser,
            },
            true,
        ) if matches!(bridge, BridgeKind::SshTerminal) => HostKeyPolicy::AcceptNew,
        (
            TrustPolicy::SshKnownHosts {
                first_use: localdesk_remote_core::FirstUsePolicy::AskUser,
            },
            false,
        ) if matches!(bridge, BridgeKind::SshTerminal) => HostKeyPolicy::Strict,
        (
            TrustPolicy::SshKnownHosts {
                first_use: localdesk_remote_core::FirstUsePolicy::AskUser,
            },
            _,
        ) => {
            return Err(error(
                RemoteErrorKind::Unsupported,
                RemoteOperation::Connect,
                bridge.first_use_confirmation_unavailable(),
                RetryDisposition::UserAction,
            ));
        }
        (
            TrustPolicy::SshKnownHosts {
                first_use: localdesk_remote_core::FirstUsePolicy::Reject,
            },
            false,
        ) => HostKeyPolicy::Strict,
        (
            TrustPolicy::SshKnownHosts {
                first_use: localdesk_remote_core::FirstUsePolicy::Reject,
            },
            true,
        ) => {
            return Err(error(
                RemoteErrorKind::InvalidInput,
                RemoteOperation::Connect,
                bridge.first_use_confirmation_unavailable(),
                RetryDisposition::Never,
            ));
        }
        _ => {
            return Err(error(
                RemoteErrorKind::InvalidInput,
                RemoteOperation::Connect,
                bridge.known_hosts_policy_required(),
                RetryDisposition::Never,
            ));
        }
    };
    let authentication = match &profile.authentication {
        Authentication::SshAgent => SshAuthentication::Agent,
        Authentication::SshKey {
            private_key,
            passphrase,
        } => {
            let secret = secrets
                .resolve(private_key)
                .await
                .map_err(map_secret_error)?;
            if secret.expose_secret().is_empty() {
                return Err(error(
                    RemoteErrorKind::InvalidInput,
                    RemoteOperation::ResolveSecret,
                    "ssh_private_key_empty",
                    RetryDisposition::Never,
                ));
            }
            let file = write_identity_file(secret.expose_secret())?;
            let path = file.path().to_owned();
            identity_files.push(file);
            if let Some(passphrase) = passphrase {
                let secret = secrets
                    .resolve(passphrase)
                    .await
                    .map_err(map_secret_error)?;
                *askpass = Some(AskpassSecret::new(secret.expose_secret())?);
                SshAuthentication::IdentityFileWithPassphrase(path)
            } else {
                SshAuthentication::IdentityFile(path)
            }
        }
        Authentication::Password { secret } => {
            let secret = secrets.resolve(secret).await.map_err(map_secret_error)?;
            *askpass = Some(AskpassSecret::new(secret.expose_secret())?);
            SshAuthentication::Password
        }
        Authentication::Anonymous | Authentication::Kerberos => {
            return Err(error(
                RemoteErrorKind::InvalidInput,
                RemoteOperation::Connect,
                bridge.authentication_invalid(),
                RetryDisposition::Never,
            ));
        }
    };
    Ok(Endpoint {
        host: profile.endpoint.host().to_owned(),
        port: profile.endpoint.port,
        user: profile.username.clone(),
        trust: HostTrust {
            known_hosts_file: trust_files.known_hosts_file.clone(),
            revoked_host_keys_file: trust_files.revoked_host_keys_file.clone(),
            policy,
        },
        authentication,
    })
}

fn write_identity_file(secret: &[u8]) -> Result<NamedTempFile, RemoteError> {
    let mut file = Builder::new()
        .prefix("localdesk-ssh-identity")
        .tempfile()
        .map_err(|_| {
            error(
                RemoteErrorKind::Transport,
                RemoteOperation::ResolveSecret,
                "ssh_identity_file_create_failed",
                RetryDisposition::Backoff,
            )
        })?;
    file.write_all(secret).map_err(|_| {
        error(
            RemoteErrorKind::Transport,
            RemoteOperation::ResolveSecret,
            "ssh_identity_file_write_failed",
            RetryDisposition::Backoff,
        )
    })?;
    file.as_file().sync_all().map_err(|_| {
        error(
            RemoteErrorKind::Transport,
            RemoteOperation::ResolveSecret,
            "ssh_identity_file_sync_failed",
            RetryDisposition::Backoff,
        )
    })?;
    Ok(file)
}

fn map_secret_error(error_value: SecretStoreError) -> RemoteError {
    let (reason_value, retry) = match error_value {
        SecretStoreError::Locked(_) => ("secret_store_locked", RetryDisposition::UserAction),
        SecretStoreError::PermissionDenied(_) => (
            "secret_store_permission_denied",
            RetryDisposition::UserAction,
        ),
        SecretStoreError::Unavailable(_) => ("secret_store_unavailable", RetryDisposition::Backoff),
        SecretStoreError::NotFound(_) => ("secret_not_found", RetryDisposition::UserAction),
        SecretStoreError::Backend(_) => ("secret_store_backend_error", RetryDisposition::Backoff),
    };
    error(
        RemoteErrorKind::SecretStore,
        RemoteOperation::ResolveSecret,
        reason_value,
        retry,
    )
}

fn map_private_profile_error(_error: crate::ProfileError) -> RemoteError {
    error(
        RemoteErrorKind::InvalidInput,
        RemoteOperation::Connect,
        "sftp_adapter_profile_invalid",
        RetryDisposition::Never,
    )
}

pub(crate) fn validate_bridge_trust(trust: &HostTrust) -> Result<(), crate::ProfileError> {
    SshProfile {
        target: Endpoint {
            host: "validation.invalid".to_owned(),
            port: 22,
            user: None,
            trust: trust.clone(),
            authentication: SshAuthentication::Agent,
        },
        jump_hosts: Vec::new(),
    }
    .validate()
}

fn executable(program: &str) -> bool {
    std::fs::metadata(program)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn sftp_capabilities() -> CapabilityMatrix {
    sftp_capabilities_with_atomic_rename(false)
}

fn sftp_session_capabilities(supports_atomic_rename: bool) -> CapabilityMatrix {
    sftp_capabilities_with_atomic_rename(supports_atomic_rename)
}

fn sftp_capabilities_with_atomic_rename(supports_atomic_rename: bool) -> CapabilityMatrix {
    CapabilityMatrix::complete(FILE_OPERATIONS.iter().copied().map(|operation| {
        let status = match operation {
            FileOperation::List
            | FileOperation::Stat
            | FileOperation::Read
            | FileOperation::Write
            | FileOperation::CreateDirectory
            | FileOperation::Rename
            | FileOperation::Delete
            | FileOperation::ResumeRead
            | FileOperation::ResumeWrite => CapabilityStatus::Supported,
            FileOperation::AtomicRename if supports_atomic_rename => CapabilityStatus::Supported,
            FileOperation::AtomicRename => {
                CapabilityStatus::Unsupported(reason("sftp_posix_rename_endpoint_dependent"))
            }
            FileOperation::SetPermissions => {
                CapabilityStatus::Unsupported(reason("sftp_set_permissions_not_implemented"))
            }
        };
        OperationCapability { operation, status }
    }))
    .expect("complete static SFTP capability matrix")
}

struct SessionInner {
    profile: SshProfile,
    _identity_files: Vec<NamedTempFile>,
    askpass: Option<AskpassSecret>,
    capabilities: CapabilityMatrix,
    session: Mutex<RemoteSession>,
    writes: Mutex<HashMap<RemoteWriteHandle, SftpWriteState>>,
    io_lock: AsyncMutex<()>,
    structured: AsyncMutex<Option<StructuredSftpSession>>,
}

#[derive(Debug, Clone)]
struct SftpWriteState {
    final_path: RemotePath,
    temporary_path: RemotePath,
    expected_size_bytes: Option<u64>,
    expected_destination: Option<ObjectIdentity>,
    next_offset: u64,
    identity: ObjectIdentity,
}

impl SessionInner {
    fn ensure_ready(&self, operation: RemoteOperation) -> Result<(), RemoteError> {
        let session = self
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if session.state != ConnectionState::Ready {
            return Err(error(
                RemoteErrorKind::Cancelled,
                operation,
                "sftp_session_not_ready",
                RetryDisposition::Never,
            ));
        }
        Ok(())
    }

    async fn structured_operation<T, F>(
        &self,
        operation: RemoteOperation,
        control: RemoteIoControl,
        task: F,
    ) -> Result<T, RemoteError>
    where
        T: Send,
        F: for<'a> FnOnce(&'a Sftp) -> StructuredOperationFuture<'a, T>,
    {
        control.check(operation)?;
        let mut structured = self.structured.lock().await;
        let connection = structured.as_ref().ok_or_else(|| {
            error(
                RemoteErrorKind::Transport,
                operation,
                "sftp_structured_session_closed",
                RetryDisposition::Backoff,
            )
        })?;
        let result = structured_sftp_operation(connection, operation, control, task).await;
        let closes_session = result.as_ref().err().is_some_and(|error| {
            matches!(
                error.kind,
                RemoteErrorKind::Cancelled | RemoteErrorKind::Timeout | RemoteErrorKind::Transport
            )
        });
        if closes_session {
            let connection = structured.take();
            drop(structured);
            if let Some(connection) = connection {
                let _ = connection.close().await;
            }
            self.mark_disconnected();
        }
        result
    }

    fn mark_disconnected(&self) {
        let mut session = self
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        session.state = ConnectionState::Disconnected;
        session.updated_at_unix_ms = unix_time_ms().max(session.updated_at_unix_ms);
    }

    async fn disconnect_controlled(&self, control: RemoteIoControl) -> Result<(), RemoteError> {
        control.check(RemoteOperation::Disconnect)?;
        let _io_guard = self.io_lock.lock().await;
        let connection = self.structured.lock().await.take();
        if let Some(connection) = connection {
            let process = connection.process();
            let close = connection.close();
            tokio::pin!(close);
            let close_result = tokio::select! {
                result = &mut close => result.map_err(|error| {
                    map_adapter_error(error, RemoteOperation::Disconnect)
                }),
                failure = wait_for_control_failure(control, RemoteOperation::Disconnect) => {
                    let _ = process.terminate().await;
                    Err(failure)
                }
            };
            if let Err(error) = close_result {
                self.mark_disconnected();
                return Err(error);
            }
        }
        self.mark_disconnected();
        Ok(())
    }

    fn execute(
        &self,
        operations: &[SftpOperation],
        operation: RemoteOperation,
    ) -> Result<SftpOutput, RemoteError> {
        self.ensure_ready(operation)?;
        execute_sftp(&self.profile, operations, self.askpass.as_ref(), operation)
    }

    async fn list_controlled(
        &self,
        path: RemotePath,
        control: RemoteIoControl,
    ) -> Result<Vec<RemoteEntry>, RemoteError> {
        let _io_guard = self.io_lock.lock().await;
        self.ensure_ready(RemoteOperation::List)?;
        let path_value = path.as_str().to_owned();
        let listed = self
            .structured_operation(RemoteOperation::List, control, move |sftp| {
                Box::pin(async move {
                    let mut fs = sftp.fs();
                    let directory = fs.open_dir(Path::new(&path_value)).await?.read_dir();
                    tokio::pin!(directory);
                    let mut entries = Vec::new();
                    while let Some(entry) =
                        std::future::poll_fn(|context| directory.as_mut().poll_next(context)).await
                    {
                        let entry = entry?;
                        if entry.filename() != Path::new(".") && entry.filename() != Path::new("..")
                        {
                            entries.push((entry.filename().to_owned(), entry.metadata()));
                        }
                    }
                    Ok(entries)
                })
            })
            .await?;
        listed
            .into_iter()
            .map(|(name, metadata)| {
                let name = name
                    .to_str()
                    .ok_or_else(|| parse_error(RemoteOperation::List))?;
                if name.is_empty() || name.contains('/') || name.chars().any(char::is_control) {
                    return Err(parse_error(RemoteOperation::List));
                }
                let child = join_remote_path(&path, name)
                    .map_err(|_| parse_error(RemoteOperation::List))?;
                Ok(structured_entry(child, metadata, self.capabilities.clone()))
            })
            .collect()
    }

    async fn stat_controlled(
        &self,
        path: RemotePath,
        control: RemoteIoControl,
    ) -> Result<RemoteEntry, RemoteError> {
        let _io_guard = self.io_lock.lock().await;
        self.ensure_ready(RemoteOperation::Stat)?;
        let path_value = path.as_str().to_owned();
        let metadata = self
            .structured_operation(RemoteOperation::Stat, control, move |sftp| {
                Box::pin(async move {
                    let mut fs = sftp.fs();
                    fs.symlink_metadata(Path::new(&path_value)).await
                })
            })
            .await?;
        Ok(structured_entry(path, metadata, self.capabilities.clone()))
    }

    fn stat(
        &self,
        path: &RemotePath,
        operation: RemoteOperation,
    ) -> Result<RemoteEntry, RemoteError> {
        let output = self.execute(
            &[SftpOperation::Stat {
                remote_path: path.as_str().to_owned(),
            }],
            operation,
        )?;
        parse_stat_output(&output.stdout, path, &self.capabilities, operation)
    }

    fn create_directory(&self, path: &RemotePath) -> Result<RemoteEntry, RemoteError> {
        let output = self.execute(
            &[
                SftpOperation::CreateDirectory {
                    remote_path: path.as_str().to_owned(),
                },
                SftpOperation::Stat {
                    remote_path: path.as_str().to_owned(),
                },
            ],
            RemoteOperation::CreateDirectory,
        )?;
        parse_stat_output(
            &output.stdout,
            path,
            &self.capabilities,
            RemoteOperation::CreateDirectory,
        )
    }

    fn rename(&self, from: &RemotePath, to: &RemotePath) -> Result<RemoteEntry, RemoteError> {
        let output = self.execute(
            &[
                SftpOperation::Rename {
                    from: from.as_str().to_owned(),
                    to: to.as_str().to_owned(),
                },
                SftpOperation::Stat {
                    remote_path: to.as_str().to_owned(),
                },
            ],
            RemoteOperation::Rename,
        )?;
        parse_stat_output(
            &output.stdout,
            to,
            &self.capabilities,
            RemoteOperation::Rename,
        )
    }

    fn delete(&self, path: &RemotePath) -> Result<(), RemoteError> {
        let entry = self.stat(path, RemoteOperation::Delete)?;
        let operation = if entry.kind == EntryKind::Directory {
            SftpOperation::RemoveDirectory {
                remote_path: path.as_str().to_owned(),
            }
        } else {
            SftpOperation::RemoveFile {
                remote_path: path.as_str().to_owned(),
            }
        };
        self.execute(&[operation], RemoteOperation::Delete)?;
        Ok(())
    }

    async fn read_chunk_controlled(
        &self,
        request: RemoteReadRequest,
        control: RemoteIoControl,
    ) -> Result<RemoteReadChunk, RemoteError> {
        if !request.is_bounded() {
            return Err(error(
                RemoteErrorKind::InvalidInput,
                RemoteOperation::Read,
                "sftp_read_chunk_size_invalid",
                RetryDisposition::Never,
            ));
        }
        let _io_guard = self.io_lock.lock().await;
        self.ensure_ready(RemoteOperation::Read)?;
        let path = request.path.clone();
        let path_value = path.as_str().to_owned();
        let offset = request.offset;
        let maximum = request.max_bytes;
        let (before, bytes, observed_eof, after) = self
            .structured_operation(RemoteOperation::Read, control, move |sftp| {
                Box::pin(async move {
                    let mut file = sftp.open(Path::new(&path_value)).await?;
                    let before = file.metadata().await?;
                    file.seek(SeekFrom::Start(offset)).await?;
                    let mut bytes = Vec::with_capacity(maximum as usize);
                    let mut observed_eof = false;
                    while bytes.len() < maximum as usize {
                        let position = offset.saturating_add(bytes.len() as u64);
                        file.seek(SeekFrom::Start(position)).await?;
                        let remaining = maximum as usize - bytes.len();
                        match file
                            .read(
                                remaining.min(u32::MAX as usize) as u32,
                                BytesMut::with_capacity(remaining),
                            )
                            .await?
                        {
                            Some(chunk) if !chunk.is_empty() => bytes.extend_from_slice(&chunk),
                            Some(_) | None => {
                                observed_eof = true;
                                break;
                            }
                        }
                    }
                    let after = file.metadata().await?;
                    file.close().await?;
                    Ok((before, bytes, observed_eof, after))
                })
            })
            .await?;
        let before = structured_identity(before);
        let after = structured_identity(after);
        ensure_identity(
            request.expected_identity.as_ref(),
            Some(&before),
            RemoteOperation::Read,
        )?;
        ensure_identity(Some(&before), Some(&after), RemoteOperation::Read)?;
        let eof = observed_eof
            || after
                .size_bytes
                .is_some_and(|size| offset.saturating_add(bytes.len() as u64) >= size);
        Ok(RemoteReadChunk {
            offset,
            bytes,
            eof,
            identity: after,
        })
    }

    async fn begin_write_controlled(
        &self,
        request: BeginWriteRequest,
        control: RemoteIoControl,
    ) -> Result<RemoteWriteReceipt, RemoteError> {
        let offset = request.resume_from.unwrap_or(0);
        if request
            .expected_size_bytes
            .is_some_and(|size| offset > size)
        {
            return Err(error(
                RemoteErrorKind::InvalidInput,
                RemoteOperation::Write,
                "sftp_resume_offset_exceeds_expected_size",
                RetryDisposition::Never,
            ));
        }
        let _io_guard = self.io_lock.lock().await;
        self.ensure_ready(RemoteOperation::Write)?;
        let final_path = request.final_path.clone();
        let temporary_path = request.temporary_path.clone();
        if final_path == temporary_path {
            return Err(error(
                RemoteErrorKind::InvalidInput,
                RemoteOperation::Write,
                "sftp_temporary_path_must_differ",
                RetryDisposition::Never,
            ));
        }
        let final_value = final_path.as_str().to_owned();
        let temporary_value = temporary_path.as_str().to_owned();
        let expected_destination = request.expected_destination.clone();
        let resume = request.resume_from.is_some();
        let result = self
            .structured_operation(RemoteOperation::Write, control, move |sftp| {
                Box::pin(async move {
                    if !sftp.support_posix_rename() {
                        return Ok(StructuredBeginResult::AtomicRenameUnavailable);
                    }
                    let mut fs = sftp.fs();
                    let destination = match fs.metadata(Path::new(&final_value)).await {
                        Ok(metadata) => Some(structured_identity(metadata)),
                        Err(error) if structured_not_found(&error) => None,
                        Err(error) => return Err(error),
                    };
                    if !identities_match(expected_destination.as_ref(), destination.as_ref()) {
                        return Ok(StructuredBeginResult::DestinationChanged);
                    }
                    let mut options = sftp.options();
                    // openssh-sftp-client requires read access for File::metadata.
                    options.read(true).write(true);
                    if !resume {
                        options.create(true).truncate(true);
                    }
                    let mut file = options.open(Path::new(&temporary_value)).await?;
                    let identity = structured_identity(file.metadata().await?);
                    file.close().await?;
                    Ok(StructuredBeginResult::Ready(identity))
                })
            })
            .await?;
        let identity = match result {
            StructuredBeginResult::AtomicRenameUnavailable => {
                return Err(error(
                    RemoteErrorKind::Unsupported,
                    RemoteOperation::Write,
                    "sftp_posix_rename_not_supported",
                    RetryDisposition::Never,
                ));
            }
            StructuredBeginResult::DestinationChanged => {
                return Err(identity_changed(RemoteOperation::Write));
            }
            StructuredBeginResult::Ready(identity) => identity,
        };
        if identity.size_bytes != Some(offset) {
            return Err(error(
                RemoteErrorKind::Conflict,
                RemoteOperation::Resume,
                "sftp_resume_offset_mismatch",
                RetryDisposition::UserAction,
            ));
        }
        let handle = RemoteWriteHandle::new();
        self.writes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                handle,
                SftpWriteState {
                    final_path,
                    temporary_path,
                    expected_size_bytes: request.expected_size_bytes,
                    expected_destination: request.expected_destination,
                    next_offset: offset,
                    identity: identity.clone(),
                },
            );
        Ok(RemoteWriteReceipt {
            handle,
            next_offset: offset,
            identity: Some(identity),
        })
    }

    async fn write_chunk_controlled(
        &self,
        handle: RemoteWriteHandle,
        offset: u64,
        bytes: Vec<u8>,
        control: RemoteIoControl,
    ) -> Result<RemoteWriteReceipt, RemoteError> {
        if bytes.is_empty() || bytes.len() > MAX_REMOTE_CHUNK_BYTES as usize {
            return Err(error(
                RemoteErrorKind::InvalidInput,
                RemoteOperation::Write,
                "sftp_write_chunk_size_invalid",
                RetryDisposition::Never,
            ));
        }
        let _io_guard = self.io_lock.lock().await;
        self.ensure_ready(RemoteOperation::Write)?;
        let state = self
            .writes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&handle)
            .cloned()
            .ok_or_else(|| write_handle_missing(RemoteOperation::Write))?;
        if state.next_offset != offset {
            return Err(error(
                RemoteErrorKind::Conflict,
                RemoteOperation::Resume,
                "sftp_write_offset_mismatch",
                RetryDisposition::UserAction,
            ));
        }
        let next_offset = offset.saturating_add(bytes.len() as u64);
        if state
            .expected_size_bytes
            .is_some_and(|size| next_offset > size)
        {
            return Err(error(
                RemoteErrorKind::InvalidInput,
                RemoteOperation::Write,
                "sftp_write_exceeds_expected_size",
                RetryDisposition::Never,
            ));
        }
        let temporary_value = state.temporary_path.as_str().to_owned();
        let identity = self
            .structured_operation(RemoteOperation::Write, control, move |sftp| {
                Box::pin(async move {
                    let mut file = sftp
                        .options()
                        .read(true)
                        .write(true)
                        .open(Path::new(&temporary_value))
                        .await?;
                    file.seek(SeekFrom::Start(offset)).await?;
                    file.write_all(&bytes).await?;
                    if sftp.support_fsync() {
                        file.sync_all().await?;
                    }
                    let identity = structured_identity(file.metadata().await?);
                    file.close().await?;
                    Ok(identity)
                })
            })
            .await?;
        if identity.size_bytes != Some(next_offset) {
            return Err(error(
                RemoteErrorKind::RemoteProtocol,
                RemoteOperation::Write,
                "sftp_written_size_mismatch",
                RetryDisposition::Never,
            ));
        }
        let mut writes = self
            .writes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let current = writes
            .get_mut(&handle)
            .ok_or_else(|| write_handle_missing(RemoteOperation::Write))?;
        if current.next_offset != offset {
            return Err(error(
                RemoteErrorKind::Conflict,
                RemoteOperation::Resume,
                "sftp_write_state_changed",
                RetryDisposition::UserAction,
            ));
        }
        current.next_offset = next_offset;
        current.identity = identity.clone();
        Ok(RemoteWriteReceipt {
            handle,
            next_offset,
            identity: Some(identity),
        })
    }

    async fn commit_write_controlled(
        &self,
        handle: RemoteWriteHandle,
        expected_identity: Option<ObjectIdentity>,
        control: RemoteIoControl,
    ) -> Result<RemoteEntry, RemoteError> {
        let _io_guard = self.io_lock.lock().await;
        self.ensure_ready(RemoteOperation::Write)?;
        let state = self
            .writes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&handle)
            .cloned()
            .ok_or_else(|| write_handle_missing(RemoteOperation::Write))?;
        if state.expected_size_bytes != Some(state.next_offset)
            && state.expected_size_bytes.is_some()
        {
            return Err(error(
                RemoteErrorKind::Conflict,
                RemoteOperation::Write,
                "sftp_commit_size_incomplete",
                RetryDisposition::UserAction,
            ));
        }
        let expected_destination = expected_identity.or(state.expected_destination.clone());
        let final_path = state.final_path.clone();
        let final_value = final_path.as_str().to_owned();
        let temporary_value = state.temporary_path.as_str().to_owned();
        let expected_temporary_size = state.next_offset;
        let outcome = self
            .structured_operation(RemoteOperation::Write, control, move |sftp| {
                Box::pin(async move {
                    if !sftp.support_posix_rename() {
                        return Ok(StructuredCommitResult::AtomicRenameUnavailable);
                    }
                    let mut fs = sftp.fs();
                    let destination = match fs.metadata(Path::new(&final_value)).await {
                        Ok(metadata) => Some(structured_identity(metadata)),
                        Err(error) if structured_not_found(&error) => None,
                        Err(error) => return Err(error),
                    };
                    if !identities_match(expected_destination.as_ref(), destination.as_ref()) {
                        return Ok(StructuredCommitResult::DestinationChanged);
                    }
                    let temporary =
                        structured_identity(fs.metadata(Path::new(&temporary_value)).await?);
                    if temporary.size_bytes != Some(expected_temporary_size) {
                        return Ok(StructuredCommitResult::TemporaryChanged);
                    }
                    fs.rename(Path::new(&temporary_value), Path::new(&final_value))
                        .await?;
                    let committed = fs.metadata(Path::new(&final_value)).await?;
                    Ok(StructuredCommitResult::Committed(committed))
                })
            })
            .await?;
        let metadata = match outcome {
            StructuredCommitResult::AtomicRenameUnavailable => {
                return Err(error(
                    RemoteErrorKind::Unsupported,
                    RemoteOperation::Write,
                    "sftp_posix_rename_not_supported",
                    RetryDisposition::Never,
                ));
            }
            StructuredCommitResult::DestinationChanged
            | StructuredCommitResult::TemporaryChanged => {
                return Err(identity_changed(RemoteOperation::Write));
            }
            StructuredCommitResult::Committed(metadata) => metadata,
        };
        self.writes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&handle);
        Ok(structured_entry(
            final_path,
            metadata,
            self.capabilities.clone(),
        ))
    }

    async fn abort_write_controlled(
        &self,
        handle: RemoteWriteHandle,
        control: RemoteIoControl,
    ) -> Result<(), RemoteError> {
        let _io_guard = self.io_lock.lock().await;
        let state = self
            .writes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&handle)
            .cloned()
            .ok_or_else(|| write_handle_missing(RemoteOperation::Write))?;
        let temporary_value = state.temporary_path.as_str().to_owned();
        self.structured_operation(RemoteOperation::Delete, control, move |sftp| {
            Box::pin(async move {
                let mut fs = sftp.fs();
                match fs.remove_file(Path::new(&temporary_value)).await {
                    Ok(()) => Ok(()),
                    Err(error) if structured_not_found(&error) => Ok(()),
                    Err(error) => Err(error),
                }
            })
        })
        .await?;
        self.writes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&handle);
        Ok(())
    }
}

enum StructuredBeginResult {
    AtomicRenameUnavailable,
    DestinationChanged,
    Ready(ObjectIdentity),
}

enum StructuredCommitResult {
    AtomicRenameUnavailable,
    DestinationChanged,
    TemporaryChanged,
    Committed(openssh_sftp_client::metadata::MetaData),
}

type StructuredOperationFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, StructuredSftpError>> + Send + 'a>>;

async fn start_structured_sftp_controlled(
    profile: &SshProfile,
    askpass: Option<&AskpassSecret>,
    operation: RemoteOperation,
    control: RemoteIoControl,
) -> Result<StructuredSftpSession, RemoteError> {
    control.check(operation)?;
    let start = OpenSshAdapter.start_structured_sftp(profile, askpass);
    tokio::pin!(start);
    tokio::select! {
        result = &mut start => result.map_err(|error| map_adapter_error(error, operation)),
        failure = wait_for_control_failure(control, operation) => Err(failure),
    }
}

async fn structured_sftp_operation<T, F>(
    session: &StructuredSftpSession,
    operation: RemoteOperation,
    control: RemoteIoControl,
    task: F,
) -> Result<T, RemoteError>
where
    T: Send,
    F: for<'a> FnOnce(&'a Sftp) -> StructuredOperationFuture<'a, T>,
{
    control.check(operation)?;
    let process = session.process();
    let outcome = {
        let future = task(session.client());
        tokio::pin!(future);
        tokio::select! {
            result = &mut future => StructuredOperationOutcome::Complete(result),
            failure = wait_for_control_failure(control, operation) => {
                StructuredOperationOutcome::Interrupted(failure)
            }
        }
    };
    if matches!(outcome, StructuredOperationOutcome::Interrupted(_)) {
        let _ = process.terminate().await;
    }
    match outcome {
        StructuredOperationOutcome::Complete(Ok(value)) => Ok(value),
        StructuredOperationOutcome::Complete(Err(error)) => {
            Err(map_structured_error(error, operation))
        }
        StructuredOperationOutcome::Interrupted(failure) => Err(failure),
    }
}

enum StructuredOperationOutcome<T> {
    Complete(Result<T, StructuredSftpError>),
    Interrupted(RemoteError),
}

async fn wait_for_control_failure(
    control: RemoteIoControl,
    operation: RemoteOperation,
) -> RemoteError {
    loop {
        if let Err(error) = control.check(operation) {
            return error;
        }
        let remaining = control.deadline().saturating_duration_since(Instant::now());
        tokio::time::sleep(remaining.min(Duration::from_millis(20))).await;
    }
}

fn default_operation_control() -> RemoteIoControl {
    RemoteIoControl::new(Instant::now() + Duration::from_secs(60))
}

fn structured_not_found(error: &StructuredSftpError) -> bool {
    matches!(
        error,
        StructuredSftpError::SftpError(SftpErrorKind::NoSuchFile, _)
    )
}

fn map_structured_error(
    error_value: StructuredSftpError,
    operation: RemoteOperation,
) -> RemoteError {
    match error_value {
        StructuredSftpError::SftpError(SftpErrorKind::NoSuchFile, _) => error(
            RemoteErrorKind::NotFound,
            operation,
            "sftp_path_not_found",
            RetryDisposition::UserAction,
        ),
        StructuredSftpError::SftpError(SftpErrorKind::PermDenied, _) => error(
            RemoteErrorKind::PermissionDenied,
            operation,
            "sftp_permission_denied",
            RetryDisposition::UserAction,
        ),
        StructuredSftpError::SftpError(SftpErrorKind::OpUnsupported, _)
        | StructuredSftpError::UnsupportedExtension(_) => error(
            RemoteErrorKind::Unsupported,
            operation,
            "sftp_operation_not_supported",
            RetryDisposition::Never,
        ),
        StructuredSftpError::IOError(error_value)
            if error_value.kind() == std::io::ErrorKind::TimedOut =>
        {
            error(
                RemoteErrorKind::Timeout,
                operation,
                "sftp_transport_timeout",
                RetryDisposition::Backoff,
            )
        }
        StructuredSftpError::IOError(_)
        | StructuredSftpError::BackgroundTaskFailure(_)
        | StructuredSftpError::TaskJoinError(_) => error(
            RemoteErrorKind::Transport,
            operation,
            "sftp_transport_failed",
            RetryDisposition::Backoff,
        ),
        StructuredSftpError::SftpError(_, _)
        | StructuredSftpError::UnsupportedSftpProtocol { .. }
        | StructuredSftpError::SftpServerHelloMsgTooLong { .. }
        | StructuredSftpError::SftpServerFailure(_)
        | StructuredSftpError::FormatError(_)
        | StructuredSftpError::AwaitableError(_)
        | StructuredSftpError::BufferTooLong(_)
        | StructuredSftpError::InvalidResponseId { .. }
        | StructuredSftpError::RecursiveErrors(_)
        | StructuredSftpError::RecursiveErrors3(_)
        | StructuredSftpError::InvalidResponse(_)
        | StructuredSftpError::HandleTooLong => error(
            RemoteErrorKind::RemoteProtocol,
            operation,
            "sftp_protocol_failed",
            RetryDisposition::Never,
        ),
        _ => error(
            RemoteErrorKind::RemoteProtocol,
            operation,
            "sftp_protocol_failed",
            RetryDisposition::Never,
        ),
    }
}

fn structured_identity(metadata: openssh_sftp_client::metadata::MetaData) -> ObjectIdentity {
    ObjectIdentity {
        size_bytes: metadata.len(),
        modified_at_unix_ms: metadata
            .modified()
            .map(|value| i64::from(value.into_raw()).saturating_mul(1_000)),
        etag: None,
    }
}

fn structured_entry(
    path: RemotePath,
    metadata: openssh_sftp_client::metadata::MetaData,
    capabilities: CapabilityMatrix,
) -> RemoteEntry {
    let kind = match metadata.file_type() {
        Some(value) if value.is_file() => EntryKind::File,
        Some(value) if value.is_dir() => EntryKind::Directory,
        Some(value) if value.is_symlink() => EntryKind::Symlink,
        _ => EntryKind::Other,
    };
    let unix_mode = metadata.permissions().map(|value| value.as_raw().bits());
    let name = path
        .as_str()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or(path.as_str())
        .to_owned();
    RemoteEntry {
        name,
        path,
        kind,
        identity: structured_identity(metadata),
        unix_mode,
        capabilities,
    }
}

fn identities_match(expected: Option<&ObjectIdentity>, actual: Option<&ObjectIdentity>) -> bool {
    let Some(expected) = expected else {
        return true;
    };
    actual.is_some_and(|actual| {
        expected
            .size_bytes
            .is_none_or(|value| actual.size_bytes == Some(value))
            && expected
                .modified_at_unix_ms
                .is_none_or(|value| actual.modified_at_unix_ms == Some(value))
            && expected
                .etag
                .as_ref()
                .is_none_or(|value| actual.etag.as_ref() == Some(value))
    })
}

fn ensure_identity(
    expected: Option<&ObjectIdentity>,
    actual: Option<&ObjectIdentity>,
    operation: RemoteOperation,
) -> Result<(), RemoteError> {
    if identities_match(expected, actual) {
        Ok(())
    } else {
        Err(identity_changed(operation))
    }
}

fn identity_changed(operation: RemoteOperation) -> RemoteError {
    error(
        RemoteErrorKind::Conflict,
        operation,
        "sftp_object_identity_changed",
        RetryDisposition::UserAction,
    )
}

fn write_handle_missing(operation: RemoteOperation) -> RemoteError {
    error(
        RemoteErrorKind::InvalidInput,
        operation,
        "sftp_write_handle_not_found",
        RetryDisposition::Never,
    )
}

#[derive(Clone)]
struct SftpRemoteFileSession {
    inner: Arc<SessionInner>,
}

impl RemoteFileSession for SftpRemoteFileSession {
    fn id(&self) -> SessionId {
        self.inner
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .id
    }

    fn snapshot(&self) -> RemoteSession {
        self.inner
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn io_control_support(&self) -> RemoteIoControlSupport {
        RemoteIoControlSupport::Supported
    }

    fn list<'a>(
        &'a self,
        path: &'a RemotePath,
    ) -> AdapterFuture<'a, Result<Vec<RemoteEntry>, RemoteError>> {
        let inner = Arc::clone(&self.inner);
        let path = path.clone();
        Box::pin(async move {
            inner
                .list_controlled(path, default_operation_control())
                .await
        })
    }

    fn stat<'a>(
        &'a self,
        path: &'a RemotePath,
    ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>> {
        let inner = Arc::clone(&self.inner);
        let path = path.clone();
        Box::pin(async move {
            inner
                .stat_controlled(path, default_operation_control())
                .await
        })
    }

    fn create_directory<'a>(
        &'a self,
        path: &'a RemotePath,
    ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>> {
        let inner = Arc::clone(&self.inner);
        let path = path.clone();
        remote_task(RemoteOperation::CreateDirectory, move || {
            inner.create_directory(&path)
        })
    }

    fn rename<'a>(
        &'a self,
        from: &'a RemotePath,
        to: &'a RemotePath,
    ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>> {
        let inner = Arc::clone(&self.inner);
        let from = from.clone();
        let to = to.clone();
        remote_task(RemoteOperation::Rename, move || inner.rename(&from, &to))
    }

    fn delete<'a>(&'a self, path: &'a RemotePath) -> AdapterFuture<'a, Result<(), RemoteError>> {
        let inner = Arc::clone(&self.inner);
        let path = path.clone();
        remote_task(RemoteOperation::Delete, move || inner.delete(&path))
    }

    fn read_chunk<'a>(
        &'a self,
        request: RemoteReadRequest,
    ) -> AdapterFuture<'a, Result<RemoteReadChunk, RemoteError>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            inner
                .read_chunk_controlled(request, default_operation_control())
                .await
        })
    }

    fn begin_write<'a>(
        &'a self,
        request: BeginWriteRequest,
    ) -> AdapterFuture<'a, Result<RemoteWriteReceipt, RemoteError>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            inner
                .begin_write_controlled(request, default_operation_control())
                .await
        })
    }

    fn write_chunk<'a>(
        &'a self,
        handle: RemoteWriteHandle,
        offset: u64,
        bytes: Vec<u8>,
    ) -> AdapterFuture<'a, Result<RemoteWriteReceipt, RemoteError>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            inner
                .write_chunk_controlled(handle, offset, bytes, default_operation_control())
                .await
        })
    }

    fn commit_write<'a>(
        &'a self,
        handle: RemoteWriteHandle,
        expected_identity: Option<ObjectIdentity>,
    ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            inner
                .commit_write_controlled(handle, expected_identity, default_operation_control())
                .await
        })
    }

    fn abort_write<'a>(
        &'a self,
        handle: RemoteWriteHandle,
    ) -> AdapterFuture<'a, Result<(), RemoteError>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            inner
                .abort_write_controlled(handle, default_operation_control())
                .await
        })
    }

    fn read_chunk_controlled<'a>(
        &'a self,
        request: RemoteReadRequest,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<RemoteReadChunk, RemoteError>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move { inner.read_chunk_controlled(request, control).await })
    }

    fn begin_write_controlled<'a>(
        &'a self,
        request: BeginWriteRequest,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<RemoteWriteReceipt, RemoteError>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move { inner.begin_write_controlled(request, control).await })
    }

    fn write_chunk_controlled<'a>(
        &'a self,
        handle: RemoteWriteHandle,
        offset: u64,
        bytes: Vec<u8>,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<RemoteWriteReceipt, RemoteError>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            inner
                .write_chunk_controlled(handle, offset, bytes, control)
                .await
        })
    }

    fn commit_write_controlled<'a>(
        &'a self,
        handle: RemoteWriteHandle,
        expected_identity: Option<ObjectIdentity>,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            inner
                .commit_write_controlled(handle, expected_identity, control)
                .await
        })
    }

    fn abort_write_controlled<'a>(
        &'a self,
        handle: RemoteWriteHandle,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<(), RemoteError>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move { inner.abort_write_controlled(handle, control).await })
    }

    fn disconnect_controlled<'a>(
        &'a self,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<(), RemoteError>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move { inner.disconnect_controlled(control).await })
    }

    fn disconnect<'a>(&'a self) -> AdapterFuture<'a, Result<(), RemoteError>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            inner
                .disconnect_controlled(default_operation_control())
                .await
        })
    }
}

fn execute_sftp(
    profile: &SshProfile,
    operations: &[SftpOperation],
    askpass: Option<&AskpassSecret>,
    operation: RemoteOperation,
) -> Result<SftpOutput, RemoteError> {
    let session = OpenSshAdapter
        .start_sftp_with_askpass(profile, operations, askpass)
        .map_err(|adapter_error| map_adapter_error(adapter_error, operation))?;
    let output = session
        .wait_with_output()
        .map_err(|adapter_error| map_adapter_error(adapter_error, operation))?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(map_sftp_failure(operation, &output))
    }
}

fn map_adapter_error(error_value: AdapterError, operation: RemoteOperation) -> RemoteError {
    match error_value {
        AdapterError::InvalidProfile(_)
        | AdapterError::EmptySftpBatch
        | AdapterError::InvalidSftpPath { .. }
        | AdapterError::InvalidLocalPath { .. } => error(
            RemoteErrorKind::InvalidInput,
            operation,
            "sftp_input_not_representable",
            RetryDisposition::Never,
        ),
        AdapterError::CreateInput(_) | AdapterError::WriteInput(_) => error(
            RemoteErrorKind::Transport,
            operation,
            "sftp_private_input_failed",
            RetryDisposition::Backoff,
        ),
        AdapterError::Pty(_) => error(
            RemoteErrorKind::Unsupported,
            operation,
            "sftp_pty_path_forbidden",
            RetryDisposition::Never,
        ),
        AdapterError::SpawnSftp(_)
        | AdapterError::InspectSftp(_)
        | AdapterError::CloseSftp(_)
        | AdapterError::StructuredSftp(_) => error(
            RemoteErrorKind::Transport,
            operation,
            "sftp_process_failed",
            RetryDisposition::Backoff,
        ),
        AdapterError::StructuredSftpHandshake { reason, .. } => {
            map_disconnect_reason(operation, reason)
        }
        AdapterError::SftpOutputLimit { .. } => error(
            RemoteErrorKind::RemoteProtocol,
            operation,
            "sftp_output_limit_exceeded",
            RetryDisposition::Never,
        ),
    }
}

fn map_disconnect_reason(
    operation: RemoteOperation,
    reason: crate::DisconnectReason,
) -> RemoteError {
    match reason {
        crate::DisconnectReason::HostKeyChanged => error(
            RemoteErrorKind::Trust,
            operation,
            "ssh_host_key_changed",
            RetryDisposition::Never,
        ),
        crate::DisconnectReason::HostKeyRevoked => error(
            RemoteErrorKind::Trust,
            operation,
            "ssh_host_key_revoked",
            RetryDisposition::Never,
        ),
        crate::DisconnectReason::HostKeyUnknown => error(
            RemoteErrorKind::Trust,
            operation,
            "ssh_host_key_unknown",
            RetryDisposition::UserAction,
        ),
        crate::DisconnectReason::AuthenticationFailed => error(
            RemoteErrorKind::Authentication,
            operation,
            "ssh_authentication_failed",
            RetryDisposition::Reauthenticate,
        ),
        crate::DisconnectReason::NetworkUnreachable => error(
            RemoteErrorKind::Transport,
            operation,
            "ssh_network_unreachable",
            RetryDisposition::Backoff,
        ),
        crate::DisconnectReason::ConnectionLost => error(
            RemoteErrorKind::Transport,
            operation,
            "ssh_connection_lost",
            RetryDisposition::Backoff,
        ),
        crate::DisconnectReason::OpenSshFailure => error(
            RemoteErrorKind::Transport,
            operation,
            "sftp_process_failed",
            RetryDisposition::Backoff,
        ),
    }
}

fn map_sftp_failure(operation: RemoteOperation, output: &SftpOutput) -> RemoteError {
    let mut bytes = Vec::with_capacity(output.stderr.len() + output.stdout.len());
    bytes.extend_from_slice(&output.stderr);
    bytes.extend_from_slice(&output.stdout);
    let transcript = String::from_utf8_lossy(&bytes);
    if transcript.contains("REMOTE HOST IDENTIFICATION HAS CHANGED") {
        return error(
            RemoteErrorKind::Trust,
            operation,
            "ssh_host_key_changed",
            RetryDisposition::Never,
        );
    }
    if transcript.contains("REVOKED HOST KEY DETECTED") || transcript.contains("revoked host key") {
        return error(
            RemoteErrorKind::Trust,
            operation,
            "ssh_host_key_revoked",
            RetryDisposition::Never,
        );
    }
    if transcript.contains("Host key verification failed") {
        return error(
            RemoteErrorKind::Trust,
            operation,
            "ssh_host_key_untrusted",
            RetryDisposition::UserAction,
        );
    }
    if transcript.contains("Permission denied (publickey)") {
        return error(
            RemoteErrorKind::Authentication,
            operation,
            "ssh_authentication_failed",
            RetryDisposition::Reauthenticate,
        );
    }
    if transcript.contains("No such file") || transcript.contains("not found") {
        return error(
            RemoteErrorKind::NotFound,
            operation,
            "sftp_path_not_found",
            RetryDisposition::Never,
        );
    }
    if transcript.contains("Permission denied") {
        return error(
            RemoteErrorKind::PermissionDenied,
            operation,
            "sftp_permission_denied",
            RetryDisposition::UserAction,
        );
    }
    if transcript.contains("timed out") {
        return error(
            RemoteErrorKind::Timeout,
            operation,
            "ssh_connection_timed_out",
            RetryDisposition::Backoff,
        );
    }
    if transcript.contains("Network is unreachable")
        || transcript.contains("No route to host")
        || transcript.contains("Could not resolve hostname")
        || transcript.contains("Connection closed")
        || transcript.contains("Connection reset")
    {
        return error(
            RemoteErrorKind::Transport,
            operation,
            "ssh_connection_unreachable",
            RetryDisposition::Backoff,
        );
    }
    error(
        RemoteErrorKind::RemoteProtocol,
        operation,
        "sftp_operation_failed",
        RetryDisposition::Backoff,
    )
}

#[cfg(test)]
fn parse_list_output(
    output: &[u8],
    parent: &RemotePath,
    capabilities: &CapabilityMatrix,
    operation: RemoteOperation,
) -> Result<Vec<RemoteEntry>, RemoteError> {
    let output = std::str::from_utf8(output).map_err(|_| parse_error(operation))?;
    let mut entries = Vec::new();
    let mut listing_lines = 0_usize;
    for line in output.lines().filter(|line| !ignored_output_line(line)) {
        listing_lines += 1;
        let mut parsed = parse_long_listing(line).ok_or_else(|| parse_error(operation))?;
        let name =
            listing_child_name(parent, &parsed.name).ok_or_else(|| parse_error(operation))?;
        if name == "." || name == ".." {
            continue;
        }
        parsed.name = name;
        let path = join_remote_path(parent, &parsed.name).map_err(|_| parse_error(operation))?;
        entries.push(parsed.into_entry(path, capabilities.clone()));
    }
    if listing_lines == 0 && !output.trim().is_empty() {
        return Err(parse_error(operation));
    }
    Ok(entries)
}

fn parse_stat_output(
    output: &[u8],
    requested: &RemotePath,
    capabilities: &CapabilityMatrix,
    operation: RemoteOperation,
) -> Result<RemoteEntry, RemoteError> {
    let output = std::str::from_utf8(output).map_err(|_| parse_error(operation))?;
    let (parent, requested_name) =
        stat_parent_and_name(requested).ok_or_else(|| parse_error(operation))?;
    let mut matched = None;
    for line in output.lines().filter(|line| !ignored_output_line(line)) {
        let mut parsed = parse_long_listing(line).ok_or_else(|| parse_error(operation))?;
        let name =
            listing_child_name(&parent, &parsed.name).ok_or_else(|| parse_error(operation))?;
        if name != requested_name {
            continue;
        }
        if matched.is_some() {
            return Err(parse_error(operation));
        }
        parsed.name = if requested.as_str() == "/" {
            "/".to_owned()
        } else {
            name
        };
        matched = Some(parsed);
    }
    let parsed = matched.ok_or_else(|| parse_error(operation))?;
    Ok(parsed.into_entry(requested.clone(), capabilities.clone()))
}

fn ignored_output_line(line: &str) -> bool {
    let line = line.trim();
    line.is_empty() || line.starts_with("Connected to ") || line.starts_with("sftp>")
}

struct ParsedListing {
    name: String,
    kind: EntryKind,
    size_bytes: Option<u64>,
    unix_mode: Option<u32>,
}

impl ParsedListing {
    fn into_entry(self, path: RemotePath, capabilities: CapabilityMatrix) -> RemoteEntry {
        RemoteEntry {
            name: self.name,
            path,
            kind: self.kind,
            identity: ObjectIdentity {
                size_bytes: self.size_bytes,
                modified_at_unix_ms: None,
                etag: None,
            },
            unix_mode: self.unix_mode,
            capabilities,
        }
    }
}

fn parse_long_listing(line: &str) -> Option<ParsedListing> {
    let (fields, mut name) = first_fields(line, 8)?;
    let permissions = fields[0];
    let kind = match permissions.as_bytes().first().copied()? {
        b'-' => EntryKind::File,
        b'd' => EntryKind::Directory,
        b'l' => EntryKind::Symlink,
        _ => EntryKind::Other,
    };
    if kind == EntryKind::Symlink
        && let Some((link_name, _target)) = name.split_once(" -> ")
    {
        name = link_name;
    }
    if name.is_empty() {
        return None;
    }
    Some(ParsedListing {
        name: name.to_owned(),
        kind,
        size_bytes: if kind == EntryKind::Directory {
            None
        } else {
            fields[4].parse().ok()
        },
        unix_mode: parse_unix_mode(permissions),
    })
}

fn listing_child_name(parent: &RemotePath, listed_name: &str) -> Option<String> {
    if !listed_name.contains('/') {
        return Some(listed_name.to_owned());
    }
    let parent = parent.as_str();
    let child = if parent == "/" {
        listed_name.strip_prefix('/')?
    } else {
        listed_name
            .strip_prefix(parent.trim_end_matches('/'))?
            .strip_prefix('/')?
    };
    (!child.is_empty() && !child.contains('/')).then(|| child.to_owned())
}

fn stat_parent_and_name(requested: &RemotePath) -> Option<(RemotePath, String)> {
    let value = requested.as_str();
    let value = value.trim_end_matches('/');
    if value.is_empty() {
        return Some((RemotePath::new("/").ok()?, ".".to_owned()));
    }
    let (parent, name) = match value.rsplit_once('/') {
        Some(("", name)) => ("/", name),
        Some((parent, name)) => (parent, name),
        None => (".", value),
    };
    if name.is_empty() {
        return None;
    }
    Some((RemotePath::new(parent).ok()?, name.to_owned()))
}

fn first_fields(line: &str, count: usize) -> Option<(Vec<&str>, &str)> {
    let bytes = line.as_bytes();
    let mut fields = Vec::with_capacity(count);
    let mut index = 0;
    for _ in 0..count {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        let start = index;
        while index < bytes.len() && !bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if start == index {
            return None;
        }
        fields.push(&line[start..index]);
    }
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    (index < bytes.len()).then_some((fields, &line[index..]))
}

fn parse_unix_mode(value: &str) -> Option<u32> {
    let bytes = value.as_bytes();
    if bytes.len() < 10 {
        return None;
    }
    let mut mode = 0_u32;
    for (index, bit) in [
        (1, 0o400),
        (2, 0o200),
        (3, 0o100),
        (4, 0o040),
        (5, 0o020),
        (6, 0o010),
        (7, 0o004),
        (8, 0o002),
        (9, 0o001),
    ] {
        if bytes[index] != b'-' && !matches!(bytes[index], b'S' | b'T') {
            mode |= bit;
        }
    }
    if matches!(bytes[3], b's' | b'S') {
        mode |= 0o4000;
    }
    if matches!(bytes[6], b's' | b'S') {
        mode |= 0o2000;
    }
    if matches!(bytes[9], b't' | b'T') {
        mode |= 0o1000;
    }
    Some(mode)
}

fn join_remote_path(
    parent: &RemotePath,
    name: &str,
) -> Result<RemotePath, localdesk_remote_core::RemotePathError> {
    let parent = parent.as_str();
    let path = if parent == "/" {
        format!("/{name}")
    } else {
        format!("{}/{name}", parent.trim_end_matches('/'))
    };
    RemotePath::new(path)
}

fn parse_error(operation: RemoteOperation) -> RemoteError {
    error(
        RemoteErrorKind::RemoteProtocol,
        operation,
        "sftp_listing_parse_failed",
        RetryDisposition::Never,
    )
}

fn reason(value: &'static str) -> SafeReason {
    SafeReason::new(value).expect("static safe reason")
}

fn error(
    kind: RemoteErrorKind,
    operation: RemoteOperation,
    reason_value: &'static str,
    retry: RetryDisposition,
) -> RemoteError {
    RemoteError::new(kind, operation, reason(reason_value), retry)
}

fn unix_time_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

struct TaskState<T> {
    result: Option<Result<T, RemoteError>>,
    waker: Option<Waker>,
}

struct RemoteTask<T> {
    state: Arc<Mutex<TaskState<T>>>,
}

impl<T> Future for RemoteTask<T> {
    type Output = Result<T, RemoteError>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(result) = state.result.take() {
            Poll::Ready(result)
        } else {
            state.waker = Some(context.waker().clone());
            Poll::Pending
        }
    }
}

fn remote_task<T, F>(
    operation: RemoteOperation,
    task: F,
) -> AdapterFuture<'static, Result<T, RemoteError>>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, RemoteError> + Send + 'static,
{
    let state = Arc::new(Mutex::new(TaskState {
        result: None,
        waker: None,
    }));
    let worker_state = Arc::clone(&state);
    if std::thread::Builder::new()
        .name("localdesk-sftp".to_owned())
        .spawn(move || {
            let result = task();
            let waker = {
                let mut state = worker_state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                state.result = Some(result);
                state.waker.take()
            };
            if let Some(waker) = waker {
                waker.wake();
            }
        })
        .is_err()
    {
        state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .result = Some(Err(error(
            RemoteErrorKind::Transport,
            operation,
            "sftp_worker_unavailable",
            RetryDisposition::Backoff,
        )));
    }
    Box::pin(RemoteTask { state })
}

#[cfg(test)]
mod tests {
    use super::*;
    use localdesk_remote_core::{
        FirstUsePolicy, ProfileId, RemoteEndpoint, SecretRef, SecretValue,
    };
    use std::{
        collections::HashMap,
        fs,
        sync::atomic::{AtomicUsize, Ordering},
    };

    fn test_trust() -> HostTrust {
        HostTrust {
            known_hosts_file: "/tmp/localdesk-bridge-known-hosts".into(),
            revoked_host_keys_file: Some("/tmp/localdesk-bridge-revoked".into()),
            policy: HostKeyPolicy::Strict,
        }
    }

    fn profile(
        protocol: RemoteProtocol,
        authentication: Authentication,
        options: ProfileOptions,
    ) -> RemoteConnectionProfile {
        RemoteConnectionProfile::new(
            ProfileId::new(),
            "bridge fixture",
            protocol,
            RemoteEndpoint::new("files.example.test", 22).expect("endpoint"),
            Some("operator".to_owned()),
            None,
            authentication,
            TrustPolicy::SshKnownHosts {
                first_use: FirstUsePolicy::Reject,
            },
            options,
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
            Box::pin(async { panic!("bridge must not delete secrets") })
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
                    error(
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
            if let Poll::Ready(value) = future.as_mut().poll(&mut context) {
                return value;
            }
            std::thread::yield_now();
        }
    }

    #[test]
    fn bridge_capabilities_are_complete_and_unsupported_reasons_are_explicit() {
        let adapter = SftpRemoteFileAdapter::new(test_trust()).expect("adapter");
        assert_eq!(adapter.protocol(), RemoteProtocol::Sftp);
        for operation in [
            FileOperation::List,
            FileOperation::Stat,
            FileOperation::Read,
            FileOperation::Write,
            FileOperation::CreateDirectory,
            FileOperation::Rename,
            FileOperation::Delete,
            FileOperation::ResumeRead,
            FileOperation::ResumeWrite,
        ] {
            assert!(adapter.capabilities().status(operation).is_supported());
        }
        for operation in [FileOperation::AtomicRename, FileOperation::SetPermissions] {
            let CapabilityStatus::Unsupported(reason) = adapter.capabilities().status(operation)
            else {
                panic!("{operation:?} must be unsupported");
            };
            assert!(!reason.as_str().is_empty());
        }
        assert!(matches!(
            adapter.availability(),
            AdapterAvailability::Healthy | AdapterAvailability::Unsupported(_)
        ));
    }

    #[test]
    fn private_key_secret_is_resolved_to_owner_only_file_and_redacted() {
        let secret_text = "private-key-material-must-not-leak";
        let secrets = ImmediateSecrets {
            value: secret_text.as_bytes().to_vec(),
            resolves: AtomicUsize::new(0),
        };
        let profile = profile(
            RemoteProtocol::Sftp,
            Authentication::SshKey {
                private_key: SecretRef::secret_service(ProfileId::new().as_uuid()),
                passphrase: None,
            },
            ProfileOptions::Sftp {
                jump_profiles: Vec::new(),
            },
        );
        let adapter = SftpRemoteFileAdapter::new(test_trust()).expect("adapter");
        let prepared = block_on(prepare_profile(&adapter, &profile, &secrets)).expect("prepare");

        assert_eq!(secrets.resolves.load(Ordering::SeqCst), 1);
        assert_eq!(prepared.identity_files.len(), 1);
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
        assert_eq!(
            fs::read(identity.path()).expect("identity"),
            secret_text.as_bytes()
        );
        let debug = format!("{prepared:?}");
        assert!(!debug.contains(secret_text));
        assert!(debug.contains("<redacted:1>"));
    }

    #[test]
    fn direct_jump_profiles_map_every_hop_without_agent_forwarding() {
        let first = profile(
            RemoteProtocol::Ssh,
            Authentication::SshAgent,
            ProfileOptions::Ssh {
                jump_profiles: Vec::new(),
                agent_forwarding: false,
            },
        );
        let second = profile(
            RemoteProtocol::Ssh,
            Authentication::SshAgent,
            ProfileOptions::Ssh {
                jump_profiles: Vec::new(),
                agent_forwarding: false,
            },
        );
        let ids = [first.id, second.id];
        let resolver = Profiles(HashMap::from([(first.id, first), (second.id, second)]));
        let adapter =
            SftpRemoteFileAdapter::with_jump_profile_resolver(test_trust(), Arc::new(resolver))
                .expect("adapter");
        let target = profile(
            RemoteProtocol::Sftp,
            Authentication::SshAgent,
            ProfileOptions::Sftp {
                jump_profiles: ids.to_vec(),
            },
        );
        let secrets = ImmediateSecrets {
            value: Vec::new(),
            resolves: AtomicUsize::new(0),
        };
        let prepared = block_on(prepare_profile(&adapter, &target, &secrets)).expect("prepare");
        assert_eq!(prepared.profile.jump_hosts.len(), 2);
        assert!(
            prepared
                .profile
                .jump_hosts
                .iter()
                .all(|hop| hop.trust.policy == HostKeyPolicy::Strict)
        );
        assert_eq!(secrets.resolves.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn ask_user_direct_secrets_and_agent_forwarding_follow_explicit_policy() {
        let secrets = ImmediateSecrets {
            value: b"not-used".to_vec(),
            resolves: AtomicUsize::new(0),
        };
        let mut ask = profile(
            RemoteProtocol::Sftp,
            Authentication::SshAgent,
            ProfileOptions::Sftp {
                jump_profiles: Vec::new(),
            },
        );
        ask.trust = TrustPolicy::SshKnownHosts {
            first_use: FirstUsePolicy::AskUser,
        };
        let adapter = SftpRemoteFileAdapter::new(test_trust()).expect("adapter");
        let error_value = block_on(prepare_profile(&adapter, &ask, &secrets)).expect_err("ask");
        assert_eq!(
            error_value.reason.as_str(),
            "sftp_first_use_confirmation_not_implemented"
        );

        let password = profile(
            RemoteProtocol::Sftp,
            Authentication::Password {
                secret: SecretRef::secret_service(ProfileId::new().as_uuid()),
            },
            ProfileOptions::Sftp {
                jump_profiles: Vec::new(),
            },
        );
        let prepared = block_on(prepare_profile(&adapter, &password, &secrets))
            .expect("direct password must be prepared through askpass");
        assert!(matches!(
            prepared.profile.target.authentication,
            SshAuthentication::Password
        ));
        assert!(prepared.askpass.is_some());
        assert_eq!(secrets.resolves.load(Ordering::SeqCst), 1);

        let passphrase = profile(
            RemoteProtocol::Sftp,
            Authentication::SshKey {
                private_key: SecretRef::secret_service(ProfileId::new().as_uuid()),
                passphrase: Some(SecretRef::secret_service(ProfileId::new().as_uuid())),
            },
            ProfileOptions::Sftp {
                jump_profiles: Vec::new(),
            },
        );
        let prepared = block_on(prepare_profile(&adapter, &passphrase, &secrets))
            .expect("direct encrypted key must be prepared through askpass");
        assert!(matches!(
            prepared.profile.target.authentication,
            SshAuthentication::IdentityFileWithPassphrase(_)
        ));
        assert_eq!(prepared.identity_files.len(), 1);
        assert!(prepared.askpass.is_some());
        assert_eq!(secrets.resolves.load(Ordering::SeqCst), 3);

        let forwarding_jump = profile(
            RemoteProtocol::Ssh,
            Authentication::SshAgent,
            ProfileOptions::Ssh {
                jump_profiles: Vec::new(),
                agent_forwarding: true,
            },
        );
        let jump_id = forwarding_jump.id;
        let resolver = Profiles(HashMap::from([(jump_id, forwarding_jump)]));
        let forwarding_adapter =
            SftpRemoteFileAdapter::with_jump_profile_resolver(test_trust(), Arc::new(resolver))
                .expect("adapter");
        let target = profile(
            RemoteProtocol::Sftp,
            Authentication::SshAgent,
            ProfileOptions::Sftp {
                jump_profiles: vec![jump_id],
            },
        );
        let error_value = block_on(prepare_profile(&forwarding_adapter, &target, &secrets))
            .expect_err("agent forwarding");
        assert_eq!(
            error_value.reason.as_str(),
            "ssh_agent_forwarding_forbidden"
        );
    }

    #[test]
    fn listing_parser_preserves_real_metadata_and_leaves_unknowns_empty() {
        let output = concat!(
            "drwxr-xr-x    2 1000 1000 4096 Aug 08 12:00 .\n",
            "drwxr-xr-x    3 1000 1000 4096 Aug 08 12:00 ..\n",
            "-rw-r-----    1 1000 1000 37 Aug 08 12:00 report  final.txt\n",
            "lrwxrwxrwx    1 1000 1000 10 Aug 08 12:00 current -> report.txt\n"
        );
        let entries = parse_list_output(
            output.as_bytes(),
            &RemotePath::new("/srv").expect("path"),
            &sftp_capabilities(),
            RemoteOperation::List,
        )
        .expect("listing");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "report  final.txt");
        assert_eq!(entries[0].identity.size_bytes, Some(37));
        assert_eq!(entries[0].identity.modified_at_unix_ms, None);
        assert_eq!(entries[0].identity.etag, None);
        assert_eq!(entries[0].unix_mode, Some(0o640));
        assert_eq!(entries[1].kind, EntryKind::Symlink);
        assert_eq!(entries[1].name, "current");
    }

    #[test]
    fn listing_parser_normalizes_openssh_absolute_names_before_joining_parent() {
        let output = concat!(
            "drwxr-xr-x    2 1000 1000 4096 Aug 11 15:00 /srv/files/.\n",
            "drwxr-xr-x    3 1000 1000 4096 Aug 11 15:00 /srv/files/..\n",
            "-rw-r--r--    1 1000 1000 15 Aug 11 15:00 /srv/files/source.txt\n"
        );
        let entries = parse_list_output(
            output.as_bytes(),
            &RemotePath::new("/srv/files").expect("path"),
            &sftp_capabilities(),
            RemoteOperation::List,
        )
        .expect("absolute listing");

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "source.txt");
        assert_eq!(entries[0].path.as_str(), "/srv/files/source.txt");
    }

    #[test]
    fn stat_parser_selects_the_requested_entry_from_its_parent_listing() {
        let output = concat!(
            "drwxr-xr-x    2 1000 1000 4096 Aug 11 15:00 /srv/.\n",
            "drwxr-xr-x    3 1000 1000 4096 Aug 11 15:00 /srv/..\n",
            "-rw-r--r--    1 1000 1000 15 Aug 11 15:00 /srv/other.txt\n",
            "-rw-r-----    1 1000 1000 21 Aug 11 15:00 /srv/requested.txt\n"
        );
        let entry = parse_stat_output(
            output.as_bytes(),
            &RemotePath::new("/srv/requested.txt").expect("path"),
            &sftp_capabilities(),
            RemoteOperation::Stat,
        )
        .expect("stat");

        assert_eq!(entry.name, "requested.txt");
        assert_eq!(entry.path.as_str(), "/srv/requested.txt");
        assert_eq!(entry.identity.size_bytes, Some(21));
        assert_eq!(entry.unix_mode, Some(0o640));
    }

    #[test]
    fn invalid_chunk_size_is_rejected_without_spawning_sftp() {
        let now = unix_time_ms();
        let session = SftpRemoteFileSession {
            inner: Arc::new(SessionInner {
                profile: SshProfile {
                    target: Endpoint {
                        host: "never-connected.invalid".to_owned(),
                        port: 22,
                        user: None,
                        trust: test_trust(),
                        authentication: SshAuthentication::Agent,
                    },
                    jump_hosts: Vec::new(),
                },
                _identity_files: Vec::new(),
                askpass: None,
                capabilities: sftp_capabilities(),
                session: Mutex::new(RemoteSession {
                    id: SessionId::new(),
                    profile_id: ProfileId::new(),
                    protocol: RemoteProtocol::Sftp,
                    state: ConnectionState::Ready,
                    capabilities: sftp_capabilities(),
                    opened_at_unix_ms: now,
                    updated_at_unix_ms: now,
                }),
                writes: Mutex::new(HashMap::new()),
                io_lock: AsyncMutex::new(()),
                structured: AsyncMutex::new(None),
            }),
        };
        let result = block_on(session.read_chunk(RemoteReadRequest {
            path: RemotePath::new("/file").expect("path"),
            offset: 0,
            max_bytes: 0,
            expected_identity: None,
        }));
        let error_value = result.expect_err("read limit must be rejected");
        assert_eq!(error_value.kind, RemoteErrorKind::InvalidInput);
        assert_eq!(error_value.reason.as_str(), "sftp_read_chunk_size_invalid");
    }

    #[tokio::test]
    async fn controlled_io_honors_cancellation_before_spawning_sftp() {
        let now = unix_time_ms();
        let session = SftpRemoteFileSession {
            inner: Arc::new(SessionInner {
                profile: SshProfile {
                    target: Endpoint {
                        host: "never-connected.invalid".to_owned(),
                        port: 22,
                        user: None,
                        trust: test_trust(),
                        authentication: SshAuthentication::Agent,
                    },
                    jump_hosts: Vec::new(),
                },
                _identity_files: Vec::new(),
                askpass: None,
                capabilities: sftp_capabilities(),
                session: Mutex::new(RemoteSession {
                    id: SessionId::new(),
                    profile_id: ProfileId::new(),
                    protocol: RemoteProtocol::Sftp,
                    state: ConnectionState::Ready,
                    capabilities: sftp_capabilities(),
                    opened_at_unix_ms: now,
                    updated_at_unix_ms: now,
                }),
                writes: Mutex::new(HashMap::new()),
                io_lock: AsyncMutex::new(()),
                structured: AsyncMutex::new(None),
            }),
        };
        let control = RemoteIoControl::new(Instant::now() + Duration::from_secs(1));
        control.cancel();
        let error_value = session
            .read_chunk_controlled(
                RemoteReadRequest {
                    path: RemotePath::new("/file").expect("path"),
                    offset: 0,
                    max_bytes: 1,
                    expected_identity: None,
                },
                control,
            )
            .await
            .expect_err("cancelled before spawn");
        assert_eq!(error_value.kind, RemoteErrorKind::Cancelled);
        assert_eq!(error_value.reason.as_str(), "remote_io_cancelled");
    }

    #[test]
    fn identity_matching_treats_unknown_fields_as_wildcards() {
        let actual = ObjectIdentity {
            size_bytes: Some(42),
            modified_at_unix_ms: Some(2_000),
            etag: None,
        };
        assert!(identities_match(
            Some(&ObjectIdentity {
                size_bytes: Some(42),
                modified_at_unix_ms: None,
                etag: None,
            }),
            Some(&actual),
        ));
        assert!(!identities_match(
            Some(&ObjectIdentity {
                size_bytes: Some(41),
                modified_at_unix_ms: None,
                etag: None,
            }),
            Some(&actual),
        ));
    }

    #[test]
    fn trust_and_authentication_failures_map_to_redacted_typed_errors() {
        use std::os::unix::process::ExitStatusExt;
        let changed = SftpOutput {
            status: std::process::ExitStatus::from_raw(255 << 8),
            stdout: Vec::new(),
            stderr: b"WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!".to_vec(),
        };
        let mapped = map_sftp_failure(RemoteOperation::Connect, &changed);
        assert_eq!(mapped.kind, RemoteErrorKind::Trust);
        assert_eq!(mapped.retry, RetryDisposition::Never);
        assert_eq!(mapped.reason.as_str(), "ssh_host_key_changed");
        assert!(!format!("{mapped:?}").contains("files.example.test"));

        let mapped = map_disconnect_reason(
            RemoteOperation::Connect,
            crate::DisconnectReason::AuthenticationFailed,
        );
        assert_eq!(mapped.kind, RemoteErrorKind::Authentication);
        assert_eq!(mapped.retry, RetryDisposition::Reauthenticate);
        assert_eq!(mapped.reason.as_str(), "ssh_authentication_failed");
    }

    #[test]
    fn remote_file_bridge_rejects_ssh_terminal_profiles_before_network_use() {
        let adapter = SftpRemoteFileAdapter::new(test_trust()).expect("adapter");
        let ssh = profile(
            RemoteProtocol::Ssh,
            Authentication::SshAgent,
            ProfileOptions::Ssh {
                jump_profiles: Vec::new(),
                agent_forwarding: false,
            },
        );
        let secrets = ImmediateSecrets {
            value: Vec::new(),
            resolves: AtomicUsize::new(0),
        };
        let result = block_on(adapter.connect(&ssh, &secrets));
        let error_value = match result {
            Ok(_) => panic!("SSH terminal profile must not enter SFTP file adapter"),
            Err(error_value) => error_value,
        };
        assert_eq!(error_value.kind, RemoteErrorKind::InvalidInput);
        assert_eq!(error_value.reason.as_str(), "sftp_profile_required");
        assert_eq!(secrets.resolves.load(Ordering::SeqCst), 0);
    }
}
