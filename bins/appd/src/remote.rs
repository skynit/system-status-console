use localdesk_domain::CapabilityRuntimeState;
use localdesk_ipc::TransferLocalHandleBind;
use localdesk_remote_core::{
    AdapterAvailability, AdapterFuture, BeginWriteRequest, CapabilityStatus, ConnectionState,
    FileOperation, MAX_SECRET_INPUT_BYTES, MAX_TERMINAL_IPC_BYTES, ObjectIdentity, ProfileId,
    ProfileOptions, RemoteAdapterCatalog, RemoteAdapterDescriptor, RemoteConnectionProfile,
    RemoteDirectoryPage, RemoteEntry, RemoteError, RemoteErrorKind, RemoteFileAdapter,
    RemoteFileSession, RemoteIoControl, RemoteIoControlSupport, RemoteOperation,
    RemoteProfileCommand, RemoteProfilePage, RemoteProfileResult, RemoteProtocol, RemoteReadChunk,
    RemoteReadRequest, RemoteSessionCommand, RemoteSessionResult, RemoteWriteHandle,
    RemoteWriteReceipt, RetryDisposition, SafeReason, SecretBackend, SecretCommand,
    SecretCommandResult, SecretKind, SecretRef, SecretStore, SecretStoreError, SecretValue,
    SessionId, StoredRemoteProfile, TerminalCapabilities as PublicTerminalCapabilities,
    TerminalCommand, TerminalData as PublicTerminalData,
    TerminalDisconnectReason as PublicDisconnectReason, TerminalRead as PublicTerminalRead,
    TerminalResult, TerminalSessionId, TerminalState as PublicTerminalState,
    TerminalStatus as PublicTerminalStatus, unsupported_file_capabilities,
};
use localdesk_remote_ftp::{PLAIN_FTP_ACKNOWLEDGEMENT, PlainFtpConfirmation, RemoteFtpAdapter};
use localdesk_remote_smb::SmbRemoteFileAdapter;
use localdesk_remote_ssh::{
    DisconnectReason, HostKeyPolicy, HostTrust, JumpProfileResolver, PtySize, SessionState,
    SftpRemoteFileAdapter, SshTerminalAdapter, SshTerminalSession, TERMINAL_CAPABILITIES,
    TerminalRead as SshTerminalRead, TerminalStatus as SshTerminalStatus,
};
use localdesk_transfers::{
    ExecutorError, ExecutorLimits, LocalHandleOwner, QueueError, QueueLimits, RemoteSessionFactory,
    RemoteTransferEndpoint, SqliteTransferStore, StoreError, TransferCommand, TransferDirection,
    TransferExecutor, TransferFeatureSet, TransferLocalHandleGrant, TransferLocalHandlePurpose,
    TransferOutput, TransferQueue, TransferStore,
};
use nix::unistd::Uid;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, DirBuilder, OpenOptions},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
    sync::{Mutex as AsyncMutex, OwnedSemaphorePermit, Semaphore, watch},
    task::{JoinHandle, JoinSet},
    time::{Duration, timeout},
};
use uuid::Uuid;

const STATE_DIRECTORY: &str = "localdesk";
const KNOWN_HOSTS_FILE: &str = "known_hosts";
const TRANSFER_DATABASE_FILE: &str = "transfers.sqlite3";
const REMOTE_DATABASE_FILE: &str = "remote.sqlite3";
const REMOTE_PROFILE_SCHEMA_VERSION: u32 = 1;
const MAX_REMOTE_PROFILE_DOCUMENT_BYTES: usize = 64 * 1024;
pub(crate) const MAX_REMOTE_SESSIONS: usize = 32;
const REMOTE_SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const REMOTE_SESSION_MAX_LEASE: Duration = Duration::from_secs(8 * 60 * 60);
const MAX_REMOTE_CLOSE_CONCURRENCY: usize = 8;
const TRANSFER_RUNNER_POLL_INTERVAL: Duration = Duration::from_millis(250);
const TRANSFER_RUNNER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(4);
const REMOTE_PROFILE_SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS remote_profiles (
    profile_id TEXT PRIMARY KEY NOT NULL,
    protocol TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 0),
    document BLOB NOT NULL,
    created_at_unix_ms INTEGER NOT NULL,
    updated_at_unix_ms INTEGER NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS remote_profiles_protocol_idx
    ON remote_profiles(protocol, profile_id);
PRAGMA user_version = 1;
"#;
const SECRET_TOOL_PROGRAM: &str = "/usr/sbin/secret-tool";
const SECRET_TOOL_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Error, Eq, PartialEq)]
#[error("remote runtime failed: {code}: {reason}")]
pub struct RemoteRuntimeError {
    pub code: String,
    pub reason: String,
    pub retryable: bool,
}

enum ShutdownSession {
    File(FileSessionEntry),
    Terminal(TerminalSessionEntry),
}

impl ShutdownSession {
    async fn close(self) {
        match self {
            Self::File(entry) => {
                let _ = entry.reaper_cancel.send(true);
                let protocol = entry.session.snapshot().protocol;
                let _ = run_session_future(protocol, move || {
                    Box::pin(async move { entry.session.disconnect().await })
                })
                .await;
            }
            Self::Terminal(entry) => {
                let _ = entry.reaper_cancel.send(true);
                let _ = tokio::task::spawn_blocking(move || {
                    entry
                        .session
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .close()
                })
                .await;
            }
        }
    }
}

fn session_expiry_wait(opened_at: Instant, last_activity: Instant) -> Duration {
    let lease_remaining = REMOTE_SESSION_MAX_LEASE.saturating_sub(opened_at.elapsed());
    let idle_remaining = REMOTE_SESSION_IDLE_TIMEOUT.saturating_sub(last_activity.elapsed());
    lease_remaining.min(idle_remaining)
}

fn session_is_expired(opened_at: Instant, last_activity: Instant) -> bool {
    opened_at.elapsed() >= REMOTE_SESSION_MAX_LEASE
        || last_activity.elapsed() >= REMOTE_SESSION_IDLE_TIMEOUT
}

impl RemoteRuntimeError {
    fn new(code: impl Into<String>, reason: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: code.into(),
            reason: reason.into(),
            retryable,
        }
    }
}

#[derive(Clone)]
pub struct RemoteRuntime {
    catalog: Arc<RemoteAdapterCatalog>,
    remote_ssh: CapabilityRuntimeState,
    remote_sftp: CapabilityRuntimeState,
    remote_ftp: CapabilityRuntimeState,
    remote_smb: CapabilityRuntimeState,
    transfer_runner: Arc<TransferRunnerControl>,
    profiles: Option<Arc<Mutex<RemoteProfileStore>>>,
    profile_transfer_gate: Arc<AsyncMutex<()>>,
    file_adapters: Arc<HashMap<RemoteProtocol, Arc<dyn RemoteFileAdapter>>>,
    sessions: Arc<AsyncMutex<HashMap<SessionId, FileSessionEntry>>>,
    terminal_adapter: Option<Arc<SshTerminalAdapter>>,
    terminal_sessions: Arc<AsyncMutex<HashMap<TerminalSessionId, TerminalSessionEntry>>>,
    session_capacity: Arc<Semaphore>,
    session_admission: Arc<AsyncMutex<bool>>,
    active_reapers: Arc<AtomicUsize>,
    secret_store: Arc<SecretToolStore>,
    #[cfg(test)]
    shutdown_probe: Option<Arc<tokio::sync::Notify>>,
}

struct FileSessionEntry {
    session: Arc<dyn RemoteFileSession>,
    _permit: OwnedSemaphorePermit,
    reaper_cancel: watch::Sender<bool>,
    opened_at: Instant,
    last_activity: Instant,
}

struct TerminalSessionEntry {
    session: Arc<Mutex<Box<dyn TerminalRuntimeSession>>>,
    _permit: OwnedSemaphorePermit,
    reaper_cancel: watch::Sender<bool>,
    opened_at: Instant,
    last_activity: Instant,
}

trait TerminalRuntimeSession: Send {
    fn read_output(&mut self, max_bytes: usize) -> Result<SshTerminalRead, RemoteError>;
    fn write_input(&mut self, bytes: &[u8]) -> Result<(), RemoteError>;
    fn resize(&self, size: PtySize) -> Result<(), RemoteError>;
    fn poll_state(&mut self) -> Result<SshTerminalStatus, RemoteError>;
    fn close(&mut self) -> Result<SshTerminalStatus, RemoteError>;
}

impl TerminalRuntimeSession for SshTerminalSession {
    fn read_output(&mut self, max_bytes: usize) -> Result<SshTerminalRead, RemoteError> {
        self.read_output(max_bytes)
    }

    fn write_input(&mut self, bytes: &[u8]) -> Result<(), RemoteError> {
        self.write_input(bytes)
    }

    fn resize(&self, size: PtySize) -> Result<(), RemoteError> {
        self.resize(size)
    }

    fn poll_state(&mut self) -> Result<SshTerminalStatus, RemoteError> {
        self.poll_state()
    }

    fn close(&mut self) -> Result<SshTerminalStatus, RemoteError> {
        self.close()
    }
}

struct CapacityBoundFileSession {
    inner: Box<dyn RemoteFileSession>,
    _permit: OwnedSemaphorePermit,
}

struct TransferRunnerControl {
    database: Option<PathBuf>,
    status: Arc<Mutex<CapabilityRuntimeState>>,
    local_handles: LocalHandleOwner,
    provider_live: AtomicBool,
    executor: AsyncMutex<Option<TransferExecutor<SqliteTransferStore>>>,
    inactive_profile_refs: Mutex<Option<HashSet<ProfileId>>>,
    shutdown_tx: watch::Sender<bool>,
    task: AsyncMutex<Option<JoinHandle<()>>>,
}

struct ActiveReaperGuard(Arc<AtomicUsize>);

impl Drop for ActiveReaperGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl TransferRunnerControl {
    fn new(
        database: Option<PathBuf>,
        status: CapabilityRuntimeState,
        inactive_profile_refs: Option<HashSet<ProfileId>>,
    ) -> Self {
        let (shutdown_tx, _) = watch::channel(false);
        Self {
            database,
            status: Arc::new(Mutex::new(status)),
            local_handles: LocalHandleOwner::default(),
            provider_live: AtomicBool::new(false),
            executor: AsyncMutex::new(None),
            inactive_profile_refs: Mutex::new(inactive_profile_refs),
            shutdown_tx,
            task: AsyncMutex::new(None),
        }
    }

    fn status(&self) -> CapabilityRuntimeState {
        self.status.lock().map_or_else(
            |_| CapabilityRuntimeState::unreachable("transfer_runner_status_unavailable"),
            |status| status.clone(),
        )
    }

    fn set_status(&self, status: CapabilityRuntimeState) {
        if let Ok(mut current) = self.status.lock() {
            *current = status;
        }
    }
}

impl RemoteFileSession for CapacityBoundFileSession {
    fn id(&self) -> SessionId {
        self.inner.id()
    }

    fn snapshot(&self) -> localdesk_remote_core::RemoteSession {
        self.inner.snapshot()
    }

    fn io_control_support(&self) -> RemoteIoControlSupport {
        self.inner.io_control_support()
    }

    fn list<'a>(
        &'a self,
        path: &'a localdesk_remote_core::RemotePath,
    ) -> AdapterFuture<'a, Result<Vec<RemoteEntry>, RemoteError>> {
        self.inner.list(path)
    }

    fn stat<'a>(
        &'a self,
        path: &'a localdesk_remote_core::RemotePath,
    ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>> {
        self.inner.stat(path)
    }

    fn create_directory<'a>(
        &'a self,
        path: &'a localdesk_remote_core::RemotePath,
    ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>> {
        self.inner.create_directory(path)
    }

    fn rename<'a>(
        &'a self,
        from: &'a localdesk_remote_core::RemotePath,
        to: &'a localdesk_remote_core::RemotePath,
    ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>> {
        self.inner.rename(from, to)
    }

    fn delete<'a>(
        &'a self,
        path: &'a localdesk_remote_core::RemotePath,
    ) -> AdapterFuture<'a, Result<(), RemoteError>> {
        self.inner.delete(path)
    }

    fn read_chunk<'a>(
        &'a self,
        request: RemoteReadRequest,
    ) -> AdapterFuture<'a, Result<RemoteReadChunk, RemoteError>> {
        self.inner.read_chunk(request)
    }

    fn begin_write<'a>(
        &'a self,
        request: BeginWriteRequest,
    ) -> AdapterFuture<'a, Result<RemoteWriteReceipt, RemoteError>> {
        self.inner.begin_write(request)
    }

    fn write_chunk<'a>(
        &'a self,
        handle: RemoteWriteHandle,
        offset: u64,
        bytes: Vec<u8>,
    ) -> AdapterFuture<'a, Result<RemoteWriteReceipt, RemoteError>> {
        self.inner.write_chunk(handle, offset, bytes)
    }

    fn commit_write<'a>(
        &'a self,
        handle: RemoteWriteHandle,
        expected_identity: Option<ObjectIdentity>,
    ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>> {
        self.inner.commit_write(handle, expected_identity)
    }

    fn abort_write<'a>(
        &'a self,
        handle: RemoteWriteHandle,
    ) -> AdapterFuture<'a, Result<(), RemoteError>> {
        self.inner.abort_write(handle)
    }

    fn read_chunk_controlled<'a>(
        &'a self,
        request: RemoteReadRequest,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<RemoteReadChunk, RemoteError>> {
        self.inner.read_chunk_controlled(request, control)
    }

    fn begin_write_controlled<'a>(
        &'a self,
        request: BeginWriteRequest,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<RemoteWriteReceipt, RemoteError>> {
        self.inner.begin_write_controlled(request, control)
    }

    fn write_chunk_controlled<'a>(
        &'a self,
        handle: RemoteWriteHandle,
        offset: u64,
        bytes: Vec<u8>,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<RemoteWriteReceipt, RemoteError>> {
        self.inner
            .write_chunk_controlled(handle, offset, bytes, control)
    }

    fn commit_write_controlled<'a>(
        &'a self,
        handle: RemoteWriteHandle,
        expected_identity: Option<ObjectIdentity>,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>> {
        self.inner
            .commit_write_controlled(handle, expected_identity, control)
    }

    fn abort_write_controlled<'a>(
        &'a self,
        handle: RemoteWriteHandle,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<(), RemoteError>> {
        self.inner.abort_write_controlled(handle, control)
    }

    fn disconnect_controlled<'a>(
        &'a self,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<(), RemoteError>> {
        self.inner.disconnect_controlled(control)
    }

    fn disconnect<'a>(&'a self) -> AdapterFuture<'a, Result<(), RemoteError>> {
        self.inner.disconnect()
    }
}

#[derive(Clone)]
struct AppdJumpProfileResolver {
    profiles: Arc<Mutex<RemoteProfileStore>>,
}

impl JumpProfileResolver for AppdJumpProfileResolver {
    fn resolve<'a>(
        &'a self,
        profile_id: ProfileId,
    ) -> AdapterFuture<'a, Result<RemoteConnectionProfile, RemoteError>> {
        let result = self
            .profiles
            .lock()
            .map_err(|_| terminal_profile_error("remote_profile_store_poisoned"))
            .and_then(|store| {
                load_profile_by_id(&store.connection, profile_id)
                    .map_err(|_| terminal_profile_error("remote_profile_store_unavailable"))?
                    .map(|stored| stored.profile)
                    .ok_or_else(|| terminal_profile_error("remote_jump_profile_not_found"))
            });
        Box::pin(async move { result })
    }
}

impl RemoteSessionFactory for RemoteRuntime {
    fn io_control_support(
        &self,
        endpoint: &RemoteTransferEndpoint,
    ) -> localdesk_remote_core::RemoteIoControlSupport {
        self.file_adapters.get(&endpoint.protocol).map_or_else(
            || {
                localdesk_remote_core::RemoteIoControlSupport::Unsupported(reason(
                    "remote_protocol_has_no_file_adapter",
                ))
            },
            |adapter| adapter.io_control_support(),
        )
    }

    fn open<'a>(
        &'a self,
        endpoint: &'a RemoteTransferEndpoint,
        control: localdesk_remote_core::RemoteIoControl,
    ) -> AdapterFuture<'a, Result<Box<dyn RemoteFileSession>, RemoteError>> {
        Box::pin(async move {
            control.check(RemoteOperation::Connect)?;
            let profile = self
                .load_profile(endpoint.profile_id)
                .await
                .map_err(map_runtime_to_remote_error)?
                .profile;
            if profile.protocol != endpoint.protocol {
                return Err(RemoteError::new(
                    RemoteErrorKind::InvalidInput,
                    RemoteOperation::Connect,
                    reason("transfer_profile_protocol_mismatch"),
                    RetryDisposition::Never,
                ));
            }
            let adapter = self
                .file_adapters
                .get(&endpoint.protocol)
                .cloned()
                .ok_or_else(|| {
                    RemoteError::new(
                        RemoteErrorKind::Unsupported,
                        RemoteOperation::Connect,
                        reason("remote_protocol_has_no_file_adapter"),
                        RetryDisposition::Never,
                    )
                })?;
            if !adapter.io_control_support().is_supported() {
                return Err(RemoteError::new(
                    RemoteErrorKind::Unsupported,
                    RemoteOperation::Connect,
                    reason("remote_io_control_not_supported"),
                    RetryDisposition::Never,
                ));
            }
            let permit = self
                .acquire_session_permit()
                .map_err(map_runtime_to_remote_error)?;
            let secrets = self.secret_store.clone();
            let (inner, permit) = if endpoint.protocol == RemoteProtocol::Sftp {
                (
                    adapter
                        .connect_controlled(&profile, secrets.as_ref(), control)
                        .await?,
                    permit,
                )
            } else {
                spawn_blocking_holding_permit(permit, move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|_| terminal_profile_error("remote_worker_failed"))?;
                    runtime.block_on(adapter.connect_controlled(
                        &profile,
                        secrets.as_ref(),
                        control,
                    ))
                })
                .await
                .map_err(|_| terminal_profile_error("remote_worker_failed"))??
            };
            let admission = self.session_admission.lock().await;
            if !*admission {
                drop(admission);
                let _ = inner.disconnect().await;
                return Err(RemoteError::new(
                    RemoteErrorKind::Cancelled,
                    RemoteOperation::Connect,
                    reason("remote_runtime_shutting_down"),
                    RetryDisposition::Never,
                ));
            }
            Ok(Box::new(CapacityBoundFileSession {
                inner,
                _permit: permit,
            }) as Box<dyn RemoteFileSession>)
        })
    }
}

impl RemoteRuntime {
    pub fn from_environment() -> Self {
        match state_base_from_environment() {
            Ok(base) => Self::from_state_base(&base),
            Err(error) => Self::without_private_state(error.reason),
        }
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn unavailable_for_test(reason: &'static str) -> Self {
        Self::without_private_state(reason)
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn from_state_base_for_test(base: &Path) -> Self {
        Self::from_state_base(base)
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn with_file_adapter_for_test(
        mut self,
        adapter: Arc<dyn RemoteFileAdapter>,
    ) -> Self {
        let mut adapters = (*self.file_adapters).clone();
        adapters.insert(adapter.protocol(), adapter);
        self.file_adapters = Arc::new(adapters);
        self
    }

    fn from_state_base(base: &Path) -> Self {
        let private = prepare_private_state(base, Uid::current().as_raw());
        let (known_hosts, transfer_database, remote_database) = match private {
            Ok(paths) => paths,
            Err(error) => return Self::without_private_state(error.reason),
        };

        let profile_store = RemoteProfileStore::open(&remote_database)
            .ok()
            .map(|store| Arc::new(Mutex::new(store)));
        let trust = HostTrust {
            known_hosts_file: known_hosts,
            revoked_host_keys_file: None,
            policy: HostKeyPolicy::Strict,
        };
        let jump_resolver = profile_store.clone().map(|profiles| {
            Arc::new(AppdJumpProfileResolver { profiles }) as Arc<dyn JumpProfileResolver>
        });
        let terminal_adapter = jump_resolver
            .clone()
            .and_then(|resolver| {
                SshTerminalAdapter::with_jump_profile_resolver(trust.clone(), resolver).ok()
            })
            .or_else(|| SshTerminalAdapter::new(trust.clone()).ok())
            .map(Arc::new);
        let terminal_availability = terminal_adapter
            .as_ref()
            .map(|adapter| adapter.availability())
            .unwrap_or_else(|| {
                AdapterAvailability::Unsupported(reason("ssh_trust_configuration_invalid"))
            });
        let ssh = RemoteAdapterDescriptor {
            protocol: RemoteProtocol::Ssh,
            availability: terminal_availability.clone(),
            terminal: match &terminal_availability {
                AdapterAvailability::Healthy | AdapterAvailability::Degraded(_) => {
                    CapabilityStatus::Supported
                }
                AdapterAvailability::Unsupported(reason)
                | AdapterAvailability::Unreachable(reason) => {
                    CapabilityStatus::Unsupported(reason.clone())
                }
            },
            file_operations: unsupported_file_capabilities(reason("ssh_terminal_only")),
        };

        let sftp_adapter = jump_resolver
            .and_then(|resolver| {
                SftpRemoteFileAdapter::with_jump_profile_resolver(trust.clone(), resolver).ok()
            })
            .or_else(|| SftpRemoteFileAdapter::new(trust).ok())
            .map(Arc::new);
        let sftp = match &sftp_adapter {
            Some(adapter) => {
                descriptor_from_file_adapter(adapter.as_ref(), "sftp_terminal_not_applicable")
            }
            None => {
                unsupported_descriptor(RemoteProtocol::Sftp, "sftp_trust_configuration_invalid")
            }
        };

        let plain_ftp_confirmation = PlainFtpConfirmation::acknowledge(PLAIN_FTP_ACKNOWLEDGEMENT)
            .expect("the built-in plain FTP acknowledgement must remain exact");
        let ftp_adapter = Arc::new(RemoteFtpAdapter::plain_ftp(plain_ftp_confirmation));
        let ftp = descriptor_from_file_adapter(ftp_adapter.as_ref(), "ftp_terminal_not_applicable");
        let ftps_adapter = Arc::new(RemoteFtpAdapter::explicit_ftps());
        let ftps =
            descriptor_from_file_adapter(ftps_adapter.as_ref(), "ftp_terminal_not_applicable");
        let smb_adapter = Arc::new(SmbRemoteFileAdapter::system());
        let smb = descriptor_from_file_adapter(smb_adapter.as_ref(), "smb_terminal_not_applicable");
        let mut file_adapters: HashMap<RemoteProtocol, Arc<dyn RemoteFileAdapter>> = HashMap::new();
        if let Some(adapter) = sftp_adapter {
            file_adapters.insert(RemoteProtocol::Sftp, adapter);
        }
        file_adapters.insert(RemoteProtocol::Ftp, ftp_adapter);
        file_adapters.insert(RemoteProtocol::FtpsExplicit, ftps_adapter);
        file_adapters.insert(RemoteProtocol::Smb, smb_adapter);

        let (remote_ssh, remote_sftp, remote_ftp, remote_smb) = if profile_store.is_some() {
            (
                map_availability(&ssh.availability),
                map_availability(&sftp.availability),
                map_availability(&ftp.availability),
                map_availability(&smb.availability),
            )
        } else {
            let unavailable =
                CapabilityRuntimeState::unreachable("remote_profile_store_unavailable");
            (
                unavailable.clone(),
                unavailable.clone(),
                unavailable.clone(),
                unavailable,
            )
        };
        let inactive_profile_refs = load_transfer_profile_references(&transfer_database).ok();
        let transfer_runner = Arc::new(TransferRunnerControl::new(
            Some(transfer_database.clone()),
            CapabilityRuntimeState::degraded(
                "transfer_runner_not_started_public_commands_unavailable",
            ),
            inactive_profile_refs,
        ));
        let catalog = RemoteAdapterCatalog::new(
            Uuid::new_v4(),
            unix_time_ms(),
            vec![ssh, sftp, ftp, ftps, smb],
        );
        debug_assert!(catalog.validate().is_ok());
        Self {
            catalog: Arc::new(catalog),
            remote_ssh,
            remote_sftp,
            remote_ftp,
            remote_smb,
            transfer_runner,
            profiles: profile_store,
            profile_transfer_gate: Arc::new(AsyncMutex::new(())),
            file_adapters: Arc::new(file_adapters),
            sessions: Arc::new(AsyncMutex::new(HashMap::new())),
            terminal_adapter,
            terminal_sessions: Arc::new(AsyncMutex::new(HashMap::new())),
            session_capacity: Arc::new(Semaphore::new(MAX_REMOTE_SESSIONS)),
            session_admission: Arc::new(AsyncMutex::new(true)),
            active_reapers: Arc::new(AtomicUsize::new(0)),
            secret_store: Arc::new(SecretToolStore::system()),
            #[cfg(test)]
            shutdown_probe: None,
        }
    }

    fn without_private_state(reason_code: impl Into<String>) -> Self {
        let reason_code = reason_code.into();
        let adapters = [
            RemoteProtocol::Ssh,
            RemoteProtocol::Sftp,
            RemoteProtocol::Ftp,
            RemoteProtocol::FtpsExplicit,
            RemoteProtocol::Smb,
        ]
        .into_iter()
        .map(|protocol| unsupported_descriptor(protocol, &reason_code))
        .collect();
        Self {
            catalog: Arc::new(RemoteAdapterCatalog::new(
                Uuid::new_v4(),
                unix_time_ms(),
                adapters,
            )),
            remote_ssh: CapabilityRuntimeState::unreachable(reason_code.clone()),
            remote_sftp: CapabilityRuntimeState::unreachable(reason_code.clone()),
            remote_ftp: CapabilityRuntimeState::unreachable(reason_code.clone()),
            remote_smb: CapabilityRuntimeState::unreachable(reason_code.clone()),
            transfer_runner: Arc::new(TransferRunnerControl::new(
                None,
                CapabilityRuntimeState::unreachable(reason_code),
                None,
            )),
            profiles: None,
            profile_transfer_gate: Arc::new(AsyncMutex::new(())),
            file_adapters: Arc::new(HashMap::new()),
            sessions: Arc::new(AsyncMutex::new(HashMap::new())),
            terminal_adapter: None,
            terminal_sessions: Arc::new(AsyncMutex::new(HashMap::new())),
            session_capacity: Arc::new(Semaphore::new(MAX_REMOTE_SESSIONS)),
            session_admission: Arc::new(AsyncMutex::new(true)),
            active_reapers: Arc::new(AtomicUsize::new(0)),
            secret_store: Arc::new(SecretToolStore::system()),
            #[cfg(test)]
            shutdown_probe: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_shutdown_probe_for_test(mut self, probe: Arc<tokio::sync::Notify>) -> Self {
        self.shutdown_probe = Some(probe);
        self
    }

    pub fn catalog(&self) -> RemoteAdapterCatalog {
        (*self.catalog).clone()
    }

    pub fn capability_states(
        &self,
    ) -> (
        CapabilityRuntimeState,
        CapabilityRuntimeState,
        CapabilityRuntimeState,
        CapabilityRuntimeState,
        CapabilityRuntimeState,
    ) {
        (
            self.remote_ssh.clone(),
            self.remote_sftp.clone(),
            self.remote_ftp.clone(),
            self.remote_smb.clone(),
            self.transfer_runner.status(),
        )
    }

    pub(crate) fn enable_transfer_provider(&self) {
        self.transfer_runner
            .provider_live
            .store(true, Ordering::Release);
        if self.transfer_runner.status().reason == "transfer_runner_active_provider_not_wired" {
            self.refresh_transfer_status();
        }
    }

    fn refresh_transfer_status(&self) {
        let status = if self.transfer_runner.provider_live.load(Ordering::Acquire) {
            CapabilityRuntimeState::healthy("transfer_runner_active_public_commands_available")
        } else {
            CapabilityRuntimeState::degraded("transfer_runner_active_provider_not_wired")
        };
        self.transfer_runner.set_status(status);
    }

    pub async fn start_transfer_runner(&self) {
        let mut task_slot = self.transfer_runner.task.lock().await;
        if task_slot.is_some() {
            return;
        }
        let Some(database) = self.transfer_runner.database.clone() else {
            self.transfer_runner
                .set_status(CapabilityRuntimeState::unreachable(
                    "transfer_queue_store_unavailable",
                ));
            return;
        };
        let queue = tokio::task::spawn_blocking(move || {
            let store = SqliteTransferStore::open(&database)?;
            let limits = QueueLimits::new(1_000, 4, 2)
                .map_err(|_| localdesk_transfers::StoreError::Unavailable)?;
            TransferQueue::open(store, limits, unix_time_ms())
                .map_err(|_| localdesk_transfers::StoreError::Unavailable)
        })
        .await;
        let Ok(Ok(queue)) = queue else {
            self.transfer_runner
                .set_status(CapabilityRuntimeState::unreachable(
                    "transfer_queue_store_unavailable",
                ));
            return;
        };
        let limits = ExecutorLimits::new(
            4,
            localdesk_transfers::DEFAULT_TRANSFER_CHUNK_BYTES,
            Duration::from_secs(30),
            Duration::from_secs(60 * 60),
        )
        .expect("static transfer executor limits are valid");
        let remote: Arc<dyn RemoteSessionFactory> = Arc::new(self.clone());
        let executor = TransferExecutor::new(
            queue,
            Arc::new(self.transfer_runner.local_handles.clone()),
            remote,
            limits,
        );
        if let Ok(references) = executor.referenced_profiles()
            && let Ok(mut inactive) = self.transfer_runner.inactive_profile_refs.lock()
        {
            *inactive = Some(references);
        }
        *self.transfer_runner.executor.lock().await = Some(executor.clone());
        self.refresh_transfer_status();
        let status = Arc::clone(&self.transfer_runner.status);
        let shutdown = self.transfer_runner.shutdown_tx.subscribe();
        let task = tokio::spawn(run_transfer_runner(executor, shutdown, status));
        *task_slot = Some(task);
    }

    async fn shutdown_transfer_runner(&self) {
        if let Some(executor) = self.transfer_runner.executor.lock().await.as_ref() {
            if let Ok(references) = executor.referenced_profiles()
                && let Ok(mut inactive) = self.transfer_runner.inactive_profile_refs.lock()
            {
                *inactive = Some(references);
            }
            if executor.request_shutdown().is_err() {
                self.transfer_runner
                    .set_status(CapabilityRuntimeState::unreachable(
                        "transfer_runner_shutdown_failed",
                    ));
            }
        }
        let _ = self.transfer_runner.shutdown_tx.send(true);
        if let Some(mut task) = self.transfer_runner.task.lock().await.take()
            && timeout(TRANSFER_RUNNER_SHUTDOWN_TIMEOUT, &mut task)
                .await
                .is_err()
        {
            task.abort();
            let _ = task.await;
        }
        self.transfer_runner.executor.lock().await.take();
        self.transfer_runner
            .set_status(CapabilityRuntimeState::degraded(
                "transfer_runner_stopped_public_commands_unavailable",
            ));
    }

    pub async fn transfer_command(
        &self,
        command: TransferCommand,
    ) -> Result<TransferOutput, RemoteRuntimeError> {
        command.validate().map_err(|_| {
            RemoteRuntimeError::new("transfer_invalid", "transfer_command_invalid", false)
        })?;
        let _profile_guard = if matches!(command, TransferCommand::Enqueue { .. }) {
            Some(self.profile_transfer_gate.lock().await)
        } else {
            None
        };
        if !self.transfer_runner.provider_live.load(Ordering::Acquire) {
            return Err(RemoteRuntimeError::new(
                "transfer_provider_unavailable",
                "transfer_provider_not_wired",
                true,
            ));
        }
        let executor = self
            .transfer_runner
            .executor
            .lock()
            .await
            .clone()
            .ok_or_else(|| {
                RemoteRuntimeError::new(
                    "transfer_provider_unavailable",
                    "transfer_runner_not_active",
                    true,
                )
            })?;
        let requested = command.clone();
        let output = match command {
            TransferCommand::Enqueue { draft } => {
                let profile = self.load_profile(draft.remote_profile_id()).await?.profile;
                let protocol = profile.protocol;
                let adapter = match protocol {
                    RemoteProtocol::Sftp
                    | RemoteProtocol::Ftp
                    | RemoteProtocol::FtpsExplicit
                    | RemoteProtocol::Smb => self
                        .file_adapters
                        .get(&protocol)
                        .cloned()
                        .ok_or_else(|| transfer_protocol_unsupported(protocol))?,
                    RemoteProtocol::Ssh => {
                        return Err(transfer_protocol_unsupported(protocol));
                    }
                };
                ensure_file_adapter_usable(adapter.as_ref())?;
                if !adapter.io_control_support().is_supported() {
                    return Err(RemoteRuntimeError::new(
                        "transfer_unsupported",
                        "remote_io_control_not_supported",
                        false,
                    ));
                }
                let required = match draft.direction {
                    TransferDirection::Upload => FileOperation::Write,
                    TransferDirection::Download => FileOperation::Read,
                };
                if let CapabilityStatus::Unsupported(reason) =
                    adapter.capabilities().status(required)
                {
                    return Err(RemoteRuntimeError::new(
                        "transfer_unsupported",
                        reason.as_str(),
                        false,
                    ));
                }
                let features = TransferFeatureSet::from_adapter(
                    draft.direction,
                    protocol,
                    adapter.capabilities(),
                );
                let task = draft
                    .into_task(protocol, features, unix_time_ms())
                    .map_err(|_| {
                        RemoteRuntimeError::new("transfer_invalid", "transfer_draft_invalid", false)
                    })?;
                TransferOutput::Task {
                    task: executor
                        .enqueue_public(task)
                        .map_err(map_transfer_executor_error)?,
                }
            }
            TransferCommand::List { query } => TransferOutput::Page {
                page: executor
                    .list_public(query)
                    .map_err(map_transfer_executor_error)?,
            },
            TransferCommand::Get { id } => TransferOutput::Task {
                task: executor
                    .get_public(id)
                    .map_err(map_transfer_executor_error)?,
            },
            TransferCommand::Cancel {
                id,
                expected_revision,
            } => TransferOutput::Mutation {
                result: executor
                    .request_cancel_public(id, expected_revision)
                    .map_err(map_transfer_executor_error)?,
            },
            TransferCommand::Retry {
                id,
                expected_revision,
            } => TransferOutput::Mutation {
                result: executor
                    .request_retry_public(id, expected_revision)
                    .map_err(map_transfer_executor_error)?,
            },
            TransferCommand::ResolveConflict {
                id,
                expected_revision,
                policy,
            } => TransferOutput::Mutation {
                result: executor
                    .resolve_conflict_public(id, expected_revision, policy)
                    .map_err(map_transfer_executor_error)?,
            },
        };
        output.validate_for(&requested).map_err(|_| {
            RemoteRuntimeError::new("transfer_invalid", "transfer_result_invalid", false)
        })?;
        Ok(output)
    }

    pub async fn bind_transfer_local_handle(
        &self,
        bind: TransferLocalHandleBind,
    ) -> Result<TransferLocalHandleGrant, RemoteRuntimeError> {
        if !bind.validate() {
            return Err(RemoteRuntimeError::new(
                "transfer_local_handle_invalid",
                "transfer_local_handle_bind_invalid",
                false,
            ));
        }
        if !self.transfer_runner.provider_live.load(Ordering::Acquire) {
            return Err(RemoteRuntimeError::new(
                "transfer_local_handle_provider_unavailable",
                "transfer_provider_not_wired",
                true,
            ));
        }
        let executor = self
            .transfer_runner
            .executor
            .lock()
            .await
            .clone()
            .ok_or_else(|| {
                RemoteRuntimeError::new(
                    "transfer_local_handle_provider_unavailable",
                    "transfer_runner_not_active",
                    true,
                )
            })?;
        executor
            .referenced_profiles()
            .map_err(map_transfer_executor_error)?;

        let (display_name, size_bytes) = local_handle_display_metadata(&bind.path, bind.purpose)?;
        let handle = localdesk_transfers::LocalFileHandle::new();
        let result = match bind.purpose {
            TransferLocalHandlePurpose::UploadSource => self
                .transfer_runner
                .local_handles
                .bind_source(handle, &bind.path),
            TransferLocalHandlePurpose::DownloadDestination => self
                .transfer_runner
                .local_handles
                .bind_destination(handle, &bind.path),
        };
        result.map_err(|error| {
            RemoteRuntimeError::new(
                "transfer_local_handle_unavailable",
                error.reason.as_str(),
                error.is_retryable(),
            )
        })?;
        let grant = TransferLocalHandleGrant {
            handle,
            purpose: bind.purpose,
            display_name,
            size_bytes,
        };
        grant.validate().map_err(|_| {
            RemoteRuntimeError::new(
                "transfer_local_handle_invalid",
                "transfer_local_handle_metadata_invalid",
                false,
            )
        })?;
        Ok(grant)
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn bind_transfer_source_for_test(
        &self,
        handle: localdesk_transfers::LocalFileHandle,
        path: &Path,
    ) -> Result<(), RemoteRuntimeError> {
        self.transfer_runner
            .local_handles
            .bind_source(handle, path)
            .map_err(map_remote_error)
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn bind_transfer_destination_for_test(
        &self,
        handle: localdesk_transfers::LocalFileHandle,
        path: &Path,
    ) -> Result<(), RemoteRuntimeError> {
        self.transfer_runner
            .local_handles
            .bind_destination(handle, path)
            .map_err(map_remote_error)
    }

    pub async fn profile_command(
        &self,
        command: RemoteProfileCommand,
    ) -> Result<RemoteProfileResult, RemoteRuntimeError> {
        command.validate().map_err(|_| remote_profile_invalid())?;
        let profile_id = match &command {
            RemoteProfileCommand::List { .. } => None,
            RemoteProfileCommand::Upsert { profile, .. } => Some(profile.id),
            RemoteProfileCommand::Delete { profile_id, .. } => Some(*profile_id),
        };
        let _profile_guard = if profile_id.is_some() {
            Some(self.profile_transfer_gate.lock().await)
        } else {
            None
        };
        let transfer_referenced = match profile_id {
            Some(profile_id) => self.transfer_profile_is_referenced(profile_id).await?,
            None => false,
        };
        let profiles = self
            .profiles
            .clone()
            .ok_or_else(profile_store_unavailable)?;
        tokio::task::spawn_blocking(move || {
            let mut store = profiles.lock().map_err(|_| {
                RemoteRuntimeError::new(
                    "remote_profile_unavailable",
                    "remote_profile_store_poisoned",
                    true,
                )
            })?;
            store.execute(command, transfer_referenced)
        })
        .await
        .map_err(|_| {
            RemoteRuntimeError::new(
                "remote_profile_unavailable",
                "remote_profile_worker_failed",
                true,
            )
        })?
    }

    async fn transfer_profile_is_referenced(
        &self,
        profile_id: ProfileId,
    ) -> Result<bool, RemoteRuntimeError> {
        if let Some(executor) = self.transfer_runner.executor.lock().await.as_ref() {
            return executor
                .references_profile(profile_id)
                .map_err(map_transfer_executor_error);
        }
        self.transfer_runner
            .inactive_profile_refs
            .lock()
            .map_err(|_| {
                RemoteRuntimeError::new(
                    "transfer_unavailable",
                    "transfer_reference_snapshot_unavailable",
                    true,
                )
            })?
            .as_ref()
            .map(|profiles| profiles.contains(&profile_id))
            .ok_or_else(|| {
                RemoteRuntimeError::new(
                    "transfer_unavailable",
                    "transfer_reference_snapshot_unavailable",
                    true,
                )
            })
    }

    pub async fn secret_command(
        &self,
        command: SecretCommand,
    ) -> Result<SecretCommandResult, RemoteRuntimeError> {
        self.secret_command_with_program(command, Path::new(SECRET_TOOL_PROGRAM))
            .await
    }

    pub async fn session_command(
        &self,
        command: RemoteSessionCommand,
    ) -> Result<RemoteSessionResult, RemoteRuntimeError> {
        command.validate().map_err(|_| {
            RemoteRuntimeError::new(
                "remote_session_invalid",
                "remote_session_command_invalid",
                false,
            )
        })?;
        match command {
            RemoteSessionCommand::Connect { profile_id } => {
                let profile = self.load_profile(profile_id).await?.profile;
                let adapter = self
                    .file_adapters
                    .get(&profile.protocol)
                    .cloned()
                    .ok_or_else(|| {
                        RemoteRuntimeError::new(
                            "remote_protocol_unsupported",
                            "remote_protocol_has_no_file_adapter",
                            false,
                        )
                    })?;
                ensure_file_adapter_usable(adapter.as_ref())?;
                let permit = self.acquire_session_permit()?;
                let secrets = self.secret_store.clone();
                let (session, permit) = if profile.protocol == RemoteProtocol::Sftp {
                    (
                        adapter
                            .connect(&profile, secrets.as_ref())
                            .await
                            .map_err(map_remote_error)?,
                        permit,
                    )
                } else {
                    spawn_blocking_holding_permit(permit, move || {
                        let runtime = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .map_err(|_| remote_worker_failed())?;
                        runtime
                            .block_on(adapter.connect(&profile, secrets.as_ref()))
                            .map_err(map_remote_error)
                    })
                    .await
                    .map_err(|_| remote_worker_failed())??
                };
                let session: Arc<dyn RemoteFileSession> = Arc::from(session);
                let snapshot = self.register_file_session(session, permit).await?;
                Ok(RemoteSessionResult::Session(snapshot))
            }
            RemoteSessionCommand::Disconnect { session_id } => {
                self.disconnect_file_session(session_id).await?;
                Ok(RemoteSessionResult::Disconnected { session_id })
            }
            RemoteSessionCommand::List { query } => {
                let session = self.session(query.session_id).await?;
                let path = query.path.clone();
                let protocol = session.snapshot().protocol;
                let entries = run_session_future(protocol, move || {
                    Box::pin(async move { session.list(&path).await })
                })
                .await?;
                if entries.iter().any(remote_entry_is_unsafe) {
                    return Err(RemoteRuntimeError::new(
                        "remote_response_invalid",
                        "remote_entry_invalid",
                        false,
                    ));
                }
                let offset = usize::try_from(query.offset).map_err(|_| {
                    RemoteRuntimeError::new(
                        "remote_session_invalid",
                        "remote_directory_offset_invalid",
                        false,
                    )
                })?;
                let end = offset
                    .saturating_add(usize::from(query.limit))
                    .min(entries.len());
                let page_entries = if offset >= entries.len() {
                    Vec::new()
                } else {
                    entries[offset..end].to_vec()
                };
                let next_offset = (end < entries.len()).then(|| {
                    u32::try_from(end).expect("directory offset began as u32 and page is bounded")
                });
                Ok(RemoteSessionResult::DirectoryPage(RemoteDirectoryPage {
                    session_id: query.session_id,
                    path: query.path,
                    offset: query.offset,
                    entries: page_entries,
                    next_offset,
                }))
            }
            RemoteSessionCommand::Stat { session_id, path } => {
                let session = self.session(session_id).await?;
                let protocol = session.snapshot().protocol;
                let entry = run_session_future(protocol, move || {
                    Box::pin(async move { session.stat(&path).await })
                })
                .await?;
                validate_remote_entry(&entry)?;
                Ok(RemoteSessionResult::Entry(entry))
            }
            RemoteSessionCommand::CreateDirectory { session_id, path } => {
                let session = self.session(session_id).await?;
                let protocol = session.snapshot().protocol;
                let entry = run_session_future(protocol, move || {
                    Box::pin(async move { session.create_directory(&path).await })
                })
                .await?;
                validate_remote_entry(&entry)?;
                Ok(RemoteSessionResult::Entry(entry))
            }
            RemoteSessionCommand::Rename {
                session_id,
                from,
                to,
            } => {
                let session = self.session(session_id).await?;
                let protocol = session.snapshot().protocol;
                let entry = run_session_future(protocol, move || {
                    Box::pin(async move { session.rename(&from, &to).await })
                })
                .await?;
                validate_remote_entry(&entry)?;
                Ok(RemoteSessionResult::Entry(entry))
            }
            RemoteSessionCommand::Delete { session_id, path } => {
                let session = self.session(session_id).await?;
                let protocol = session.snapshot().protocol;
                run_session_future(protocol, move || {
                    Box::pin(async move { session.delete(&path).await })
                })
                .await?;
                Ok(RemoteSessionResult::Deleted { session_id })
            }
        }
    }

    pub async fn terminal_command(
        &self,
        command: TerminalCommand,
    ) -> Result<TerminalResult, RemoteRuntimeError> {
        command.validate().map_err(|_| {
            RemoteRuntimeError::new("terminal_invalid", "terminal_command_invalid", false)
        })?;
        match command {
            TerminalCommand::Open {
                profile_id,
                size,
                accept_new_host_key,
            } => {
                let permit = self.acquire_session_permit()?;
                let profile = self.load_profile(profile_id).await?.profile;
                let adapter = self.terminal_adapter.clone().ok_or_else(|| {
                    RemoteRuntimeError::new(
                        "terminal_unsupported",
                        "ssh_terminal_adapter_unavailable",
                        false,
                    )
                })?;
                let secrets = self.secret_store.clone();
                let pty_size = PtySize::with_pixels(
                    size.rows,
                    size.columns,
                    size.pixel_width,
                    size.pixel_height,
                )
                .map_err(|_| {
                    RemoteRuntimeError::new("terminal_invalid", "ssh_terminal_size_invalid", false)
                })?;
                let (session, permit) = spawn_blocking_holding_permit(permit, move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|_| remote_worker_failed())?;
                    runtime
                        .block_on(adapter.open(
                            &profile,
                            secrets.as_ref(),
                            pty_size,
                            accept_new_host_key,
                        ))
                        .map_err(map_remote_error)
                })
                .await
                .map_err(|_| remote_worker_failed())??;
                let session: Box<dyn TerminalRuntimeSession> = Box::new(session);
                let session = Arc::new(Mutex::new(session));
                let session_id = self.register_terminal_session(session, permit).await?;
                Ok(TerminalResult::Opened {
                    session_id,
                    capabilities: map_terminal_capabilities(),
                    status: PublicTerminalStatus {
                        state: PublicTerminalState::Running,
                        transcript_retained_bytes: 0,
                        transcript_dropped_bytes: 0,
                    },
                })
            }
            TerminalCommand::Read {
                session_id,
                max_bytes,
            } => {
                let session = self.terminal_session(session_id).await?;
                let output = tokio::task::spawn_blocking(move || {
                    session
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .read_output(max_bytes as usize)
                        .map_err(map_remote_error)
                })
                .await
                .map_err(|_| remote_worker_failed())??;
                let output = match output {
                    SshTerminalRead::Pending => PublicTerminalRead::Pending,
                    SshTerminalRead::Data(data) => PublicTerminalRead::Data(
                        PublicTerminalData::from_bytes(data.as_bytes()).map_err(|_| {
                            RemoteRuntimeError::new(
                                "terminal_response_invalid",
                                "terminal_output_invalid",
                                false,
                            )
                        })?,
                    ),
                    SshTerminalRead::EndOfStream => PublicTerminalRead::EndOfStream,
                };
                Ok(TerminalResult::Read { session_id, output })
            }
            TerminalCommand::Stream { .. } => Err(RemoteRuntimeError::new(
                "terminal_invalid",
                "terminal_stream_requires_ipc_server",
                false,
            )),
            TerminalCommand::Write { session_id, data } => {
                let bytes = data.decode().map_err(|_| {
                    RemoteRuntimeError::new("terminal_invalid", "terminal_input_invalid", false)
                })?;
                let accepted_bytes = u32::try_from(bytes.len()).expect("terminal input is bounded");
                let session = self.active_terminal_session(session_id).await?;
                tokio::task::spawn_blocking(move || {
                    session
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .write_input(&bytes)
                        .map_err(map_remote_error)
                })
                .await
                .map_err(|_| remote_worker_failed())??;
                Ok(TerminalResult::Wrote {
                    session_id,
                    accepted_bytes,
                })
            }
            TerminalCommand::Resize { session_id, size } => {
                let pty_size = PtySize::with_pixels(
                    size.rows,
                    size.columns,
                    size.pixel_width,
                    size.pixel_height,
                )
                .map_err(|_| {
                    RemoteRuntimeError::new("terminal_invalid", "ssh_terminal_size_invalid", false)
                })?;
                let session = self.active_terminal_session(session_id).await?;
                tokio::task::spawn_blocking(move || {
                    session
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .resize(pty_size)
                        .map_err(map_remote_error)
                })
                .await
                .map_err(|_| remote_worker_failed())??;
                Ok(TerminalResult::Resized { session_id })
            }
            TerminalCommand::Poll { session_id } => {
                let session = self.terminal_session(session_id).await?;
                let status = tokio::task::spawn_blocking(move || {
                    session
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .poll_state()
                        .map_err(map_remote_error)
                })
                .await
                .map_err(|_| remote_worker_failed())??;
                Ok(TerminalResult::Status {
                    session_id,
                    status: map_terminal_status(status)?,
                })
            }
            TerminalCommand::Close { session_id } => {
                let status = self.close_terminal_session(session_id).await?;
                Ok(TerminalResult::Closed {
                    session_id,
                    status: map_terminal_status(status)?,
                })
            }
        }
    }

    async fn load_profile(
        &self,
        profile_id: ProfileId,
    ) -> Result<StoredRemoteProfile, RemoteRuntimeError> {
        let profiles = self
            .profiles
            .clone()
            .ok_or_else(profile_store_unavailable)?;
        tokio::task::spawn_blocking(move || {
            let store = profiles.lock().map_err(|_| profile_store_unavailable())?;
            load_profile_by_id(&store.connection, profile_id)?.ok_or_else(|| {
                RemoteRuntimeError::new(
                    "remote_profile_not_found",
                    "remote_profile_not_found",
                    false,
                )
            })
        })
        .await
        .map_err(|_| remote_worker_failed())?
    }

    async fn register_file_session(
        &self,
        session: Arc<dyn RemoteFileSession>,
        permit: OwnedSemaphorePermit,
    ) -> Result<localdesk_remote_core::RemoteSession, RemoteRuntimeError> {
        let snapshot = session.snapshot();
        if !session_snapshot_is_usable(&snapshot) {
            let _ = run_session_future(snapshot.protocol, move || {
                Box::pin(async move { session.disconnect().await })
            })
            .await;
            return Err(RemoteRuntimeError::new(
                "remote_unsupported",
                "remote_session_not_usable",
                false,
            ));
        }
        let session_id = session.id();
        let (reaper_cancel, reaper_shutdown) = watch::channel(false);
        let mut permit = Some(permit);
        let rejection = {
            let admission = self.session_admission.lock().await;
            if !*admission {
                Some(remote_runtime_shutting_down())
            } else {
                let mut sessions = self.sessions.lock().await;
                match sessions.entry(session_id) {
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        let now = Instant::now();
                        entry.insert(FileSessionEntry {
                            session: session.clone(),
                            _permit: permit.take().expect("permit is inserted once"),
                            reaper_cancel,
                            opened_at: now,
                            last_activity: now,
                        });
                        None
                    }
                    std::collections::hash_map::Entry::Occupied(_) => {
                        Some(RemoteRuntimeError::new(
                            "remote_session_conflict",
                            "remote_session_id_collision",
                            false,
                        ))
                    }
                }
            }
        };
        if let Some(error) = rejection {
            let protocol = session.snapshot().protocol;
            let _ = run_session_future(protocol, move || {
                Box::pin(async move { session.disconnect().await })
            })
            .await;
            return Err(error);
        }
        self.spawn_file_session_reaper(session_id, reaper_shutdown);
        Ok(snapshot)
    }

    async fn disconnect_file_session(
        &self,
        session_id: SessionId,
    ) -> Result<(), RemoteRuntimeError> {
        let entry = self
            .sessions
            .lock()
            .await
            .remove(&session_id)
            .ok_or_else(|| {
                RemoteRuntimeError::new(
                    "remote_session_not_found",
                    "remote_session_not_found",
                    false,
                )
            })?;
        let _ = entry.reaper_cancel.send(true);
        let protocol = entry.session.snapshot().protocol;
        run_session_future(protocol, move || {
            Box::pin(async move { entry.session.disconnect().await })
        })
        .await
    }

    async fn register_terminal_session(
        &self,
        session: Arc<Mutex<Box<dyn TerminalRuntimeSession>>>,
        permit: OwnedSemaphorePermit,
    ) -> Result<TerminalSessionId, RemoteRuntimeError> {
        let session_id = TerminalSessionId::new();
        let (reaper_cancel, reaper_shutdown) = watch::channel(false);
        let mut permit = Some(permit);
        let rejection = {
            let admission = self.session_admission.lock().await;
            if !*admission {
                Some(remote_runtime_shutting_down())
            } else {
                let mut terminals = self.terminal_sessions.lock().await;
                match terminals.entry(session_id) {
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        let now = Instant::now();
                        entry.insert(TerminalSessionEntry {
                            session: Arc::clone(&session),
                            _permit: permit.take().expect("permit is inserted once"),
                            reaper_cancel,
                            opened_at: now,
                            last_activity: now,
                        });
                        None
                    }
                    std::collections::hash_map::Entry::Occupied(_) => {
                        Some(RemoteRuntimeError::new(
                            "remote_session_conflict",
                            "terminal_session_id_collision",
                            false,
                        ))
                    }
                }
            }
        };
        if let Some(error) = rejection {
            let _ = tokio::task::spawn_blocking(move || {
                session
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .close()
            })
            .await;
            return Err(error);
        }
        self.spawn_terminal_session_reaper(session_id, reaper_shutdown);
        Ok(session_id)
    }

    async fn close_terminal_session(
        &self,
        session_id: TerminalSessionId,
    ) -> Result<SshTerminalStatus, RemoteRuntimeError> {
        let entry = self
            .terminal_sessions
            .lock()
            .await
            .remove(&session_id)
            .ok_or_else(terminal_session_not_found)?;
        let _ = entry.reaper_cancel.send(true);
        tokio::task::spawn_blocking(move || {
            entry
                .session
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .close()
                .map_err(map_remote_error)
        })
        .await
        .map_err(|_| remote_worker_failed())?
    }

    async fn session(
        &self,
        session_id: SessionId,
    ) -> Result<Arc<dyn RemoteFileSession>, RemoteRuntimeError> {
        let mut sessions = self.sessions.lock().await;
        sessions
            .get_mut(&session_id)
            .map(|entry| {
                entry.last_activity = Instant::now();
                Arc::clone(&entry.session)
            })
            .ok_or_else(|| {
                RemoteRuntimeError::new(
                    "remote_session_not_found",
                    "remote_session_not_found",
                    false,
                )
            })
    }

    async fn terminal_session(
        &self,
        session_id: TerminalSessionId,
    ) -> Result<Arc<Mutex<Box<dyn TerminalRuntimeSession>>>, RemoteRuntimeError> {
        let sessions = self.terminal_sessions.lock().await;
        sessions
            .get(&session_id)
            .map(|entry| Arc::clone(&entry.session))
            .ok_or_else(terminal_session_not_found)
    }

    async fn active_terminal_session(
        &self,
        session_id: TerminalSessionId,
    ) -> Result<Arc<Mutex<Box<dyn TerminalRuntimeSession>>>, RemoteRuntimeError> {
        let mut sessions = self.terminal_sessions.lock().await;
        sessions
            .get_mut(&session_id)
            .map(|entry| {
                entry.last_activity = Instant::now();
                Arc::clone(&entry.session)
            })
            .ok_or_else(terminal_session_not_found)
    }

    fn acquire_session_permit(&self) -> Result<OwnedSemaphorePermit, RemoteRuntimeError> {
        Arc::clone(&self.session_capacity)
            .try_acquire_owned()
            .map_err(|_| {
                RemoteRuntimeError::new(
                    "remote_session_busy",
                    "remote_session_capacity_exceeded",
                    true,
                )
            })
    }

    async fn close_session_admission(&self) {
        let mut admission = self.session_admission.lock().await;
        *admission = false;
        self.session_capacity.close();
    }

    fn spawn_file_session_reaper(&self, session_id: SessionId, shutdown: watch::Receiver<bool>) {
        let runtime = self.clone();
        self.active_reapers.fetch_add(1, Ordering::AcqRel);
        let active_reapers = Arc::clone(&self.active_reapers);
        tokio::spawn(async move {
            let _guard = ActiveReaperGuard(active_reapers);
            runtime
                .reap_file_session_when_idle(session_id, shutdown)
                .await;
        });
    }

    async fn reap_file_session_when_idle(
        &self,
        session_id: SessionId,
        mut shutdown: watch::Receiver<bool>,
    ) {
        loop {
            let wait = {
                let sessions = self.sessions.lock().await;
                let Some(entry) = sessions.get(&session_id) else {
                    return;
                };
                session_expiry_wait(entry.opened_at, entry.last_activity)
            };
            if !wait.is_zero() {
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    changed = shutdown.changed() => {
                        if changed.is_err() || *shutdown.borrow() {
                            return;
                        }
                    }
                }
                continue;
            }
            let entry = {
                let mut sessions = self.sessions.lock().await;
                if sessions
                    .get(&session_id)
                    .is_some_and(|entry| session_is_expired(entry.opened_at, entry.last_activity))
                {
                    sessions.remove(&session_id)
                } else {
                    None
                }
            };
            if let Some(entry) = entry {
                let protocol = entry.session.snapshot().protocol;
                let _ = run_session_future(protocol, move || {
                    Box::pin(async move { entry.session.disconnect().await })
                })
                .await;
                return;
            }
        }
    }

    fn spawn_terminal_session_reaper(
        &self,
        session_id: TerminalSessionId,
        shutdown: watch::Receiver<bool>,
    ) {
        let runtime = self.clone();
        self.active_reapers.fetch_add(1, Ordering::AcqRel);
        let active_reapers = Arc::clone(&self.active_reapers);
        tokio::spawn(async move {
            let _guard = ActiveReaperGuard(active_reapers);
            runtime
                .reap_terminal_session_when_idle(session_id, shutdown)
                .await;
        });
    }

    async fn reap_terminal_session_when_idle(
        &self,
        session_id: TerminalSessionId,
        mut shutdown: watch::Receiver<bool>,
    ) {
        loop {
            let wait = {
                let sessions = self.terminal_sessions.lock().await;
                let Some(entry) = sessions.get(&session_id) else {
                    return;
                };
                session_expiry_wait(entry.opened_at, entry.last_activity)
            };
            if !wait.is_zero() {
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    changed = shutdown.changed() => {
                        if changed.is_err() || *shutdown.borrow() {
                            return;
                        }
                    }
                }
                continue;
            }
            let entry = {
                let mut sessions = self.terminal_sessions.lock().await;
                if sessions
                    .get(&session_id)
                    .is_some_and(|entry| session_is_expired(entry.opened_at, entry.last_activity))
                {
                    sessions.remove(&session_id)
                } else {
                    None
                }
            };
            if let Some(entry) = entry {
                let _ = tokio::task::spawn_blocking(move || {
                    entry
                        .session
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .close()
                })
                .await;
                return;
            }
        }
    }

    pub async fn shutdown_sessions(&self) {
        #[cfg(test)]
        if let Some(probe) = &self.shutdown_probe {
            probe.notify_one();
        }
        self.close_session_admission().await;
        self.shutdown_transfer_runner().await;
        let file_sessions = {
            let mut sessions = self.sessions.lock().await;
            let sessions = sessions
                .drain()
                .map(|(_, session)| session)
                .collect::<Vec<_>>();
            for session in &sessions {
                let _ = session.reaper_cancel.send(true);
            }
            sessions
        };
        let terminal_sessions = {
            let mut sessions = self.terminal_sessions.lock().await;
            let sessions = sessions
                .drain()
                .map(|(_, session)| session)
                .collect::<Vec<_>>();
            for session in &sessions {
                let _ = session.reaper_cancel.send(true);
            }
            sessions
        };
        let mut pending = file_sessions
            .into_iter()
            .map(ShutdownSession::File)
            .chain(terminal_sessions.into_iter().map(ShutdownSession::Terminal));
        let mut tasks = JoinSet::new();
        loop {
            while tasks.len() < MAX_REMOTE_CLOSE_CONCURRENCY {
                let Some(session) = pending.next() else {
                    break;
                };
                tasks.spawn(async move { session.close().await });
            }
            if tasks.join_next().await.is_none() {
                break;
            }
        }
    }

    async fn secret_command_with_program(
        &self,
        command: SecretCommand,
        program: &Path,
    ) -> Result<SecretCommandResult, RemoteRuntimeError> {
        command.validate().map_err(|_| secret_input_invalid())?;
        if !is_executable(program) {
            return Err(RemoteRuntimeError::new(
                "secret_store_unavailable",
                "secret_tool_not_installed",
                false,
            ));
        }
        match command {
            SecretCommand::Store { kind, value } => {
                if std::str::from_utf8(value.expose_secret()).is_err() {
                    return Err(secret_input_invalid());
                }
                let item_id = Uuid::new_v4();
                let item_id_string = item_id.to_string();
                let mut command = secret_tool_command(program);
                command
                    .args([
                        "store",
                        "--label=LocalDesk remote credential",
                        "localdesk-item-id",
                        item_id_string.as_str(),
                        "localdesk-secret-kind",
                        secret_kind_code(kind),
                    ])
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                let mut child =
                    spawn_secret_tool(&mut command).map_err(|_| secret_store_unavailable())?;
                let mut stdin = child.stdin.take().ok_or_else(secret_store_unavailable)?;
                stdin
                    .write_all(value.expose_secret())
                    .await
                    .map_err(|_| secret_store_unavailable())?;
                stdin
                    .shutdown()
                    .await
                    .map_err(|_| secret_store_unavailable())?;
                drop(stdin);
                wait_secret_tool(&mut child).await?;
                Ok(SecretCommandResult::Stored {
                    reference: localdesk_remote_core::SecretRef::secret_service(item_id),
                })
            }
            SecretCommand::Delete { reference } => {
                if reference.backend() != SecretBackend::SecretService {
                    return Err(secret_input_invalid());
                }
                let item_id_string = reference.item_id().to_string();
                let mut command = secret_tool_command(program);
                command
                    .args(["clear", "localdesk-item-id", item_id_string.as_str()])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                let mut child =
                    spawn_secret_tool(&mut command).map_err(|_| secret_store_unavailable())?;
                wait_secret_tool(&mut child).await?;
                Ok(SecretCommandResult::Deleted)
            }
        }
    }
}

async fn spawn_blocking_holding_permit<T, E, F>(
    permit: OwnedSemaphorePermit,
    operation: F,
) -> Result<Result<(T, OwnedSemaphorePermit), E>, tokio::task::JoinError>
where
    T: Send + 'static,
    E: Send + 'static,
    F: FnOnce() -> Result<T, E> + Send + 'static,
{
    tokio::task::spawn_blocking(move || operation().map(|value| (value, permit))).await
}

async fn run_session_future<T, F>(
    protocol: RemoteProtocol,
    operation: F,
) -> Result<T, RemoteRuntimeError>
where
    T: Send + 'static,
    F: FnOnce() -> AdapterFuture<'static, Result<T, RemoteError>> + Send + 'static,
{
    if protocol == RemoteProtocol::Sftp {
        return operation().await.map_err(map_remote_error);
    }
    tokio::task::spawn_blocking(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| remote_worker_failed())?;
        runtime.block_on(operation()).map_err(map_remote_error)
    })
    .await
    .map_err(|_| remote_worker_failed())?
}

async fn run_transfer_runner(
    executor: TransferExecutor<SqliteTransferStore>,
    mut shutdown: watch::Receiver<bool>,
    status: Arc<Mutex<CapabilityRuntimeState>>,
) {
    loop {
        if *shutdown.borrow() {
            return;
        }
        if executor.run_ready().await.is_err() {
            if let Ok(mut current) = status.lock() {
                *current = CapabilityRuntimeState::unreachable("transfer_runner_failed");
            }
            return;
        }
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
            () = tokio::time::sleep(TRANSFER_RUNNER_POLL_INTERVAL) => {}
        }
    }
}

fn validate_remote_entry(
    entry: &localdesk_remote_core::RemoteEntry,
) -> Result<(), RemoteRuntimeError> {
    if remote_entry_is_unsafe(entry) {
        Err(RemoteRuntimeError::new(
            "remote_response_invalid",
            "remote_entry_invalid",
            false,
        ))
    } else {
        Ok(())
    }
}

fn remote_entry_is_unsafe(entry: &localdesk_remote_core::RemoteEntry) -> bool {
    entry.name.is_empty()
        || entry.name.len() > 1_024
        || entry.name.chars().any(char::is_control)
        || entry
            .identity
            .etag
            .as_ref()
            .is_some_and(|etag| etag.len() > 1_024 || etag.chars().any(char::is_control))
}

fn map_remote_error(error: RemoteError) -> RemoteRuntimeError {
    let code = match error.kind {
        RemoteErrorKind::Transport => "remote_transport_error",
        RemoteErrorKind::Trust => "remote_trust_error",
        RemoteErrorKind::Authentication => "remote_authentication_error",
        RemoteErrorKind::PermissionDenied => "remote_permission_denied",
        RemoteErrorKind::NotFound => "remote_not_found",
        RemoteErrorKind::Conflict => "remote_conflict",
        RemoteErrorKind::Unsupported => "remote_unsupported",
        RemoteErrorKind::RateLimited => "remote_rate_limited",
        RemoteErrorKind::Timeout => "remote_timeout",
        RemoteErrorKind::RemoteProtocol => "remote_protocol_error",
        RemoteErrorKind::Cancelled => "remote_cancelled",
        RemoteErrorKind::InvalidInput => "remote_invalid_input",
        RemoteErrorKind::SecretStore => "remote_secret_store_error",
    };
    RemoteRuntimeError::new(code, error.reason.as_str(), error.is_retryable())
}

fn map_runtime_to_remote_error(error: RemoteRuntimeError) -> RemoteError {
    let kind = match error.code.as_str() {
        "remote_profile_not_found" => RemoteErrorKind::NotFound,
        "remote_protocol_unsupported" => RemoteErrorKind::Unsupported,
        "remote_session_busy" => RemoteErrorKind::RateLimited,
        "remote_profile_conflict" => RemoteErrorKind::Conflict,
        _ => RemoteErrorKind::Transport,
    };
    let safe_reason =
        SafeReason::new(error.reason).unwrap_or_else(|_| reason("remote_runtime_error"));
    RemoteError::new(
        kind,
        RemoteOperation::Connect,
        safe_reason,
        if error.retryable {
            RetryDisposition::Backoff
        } else {
            RetryDisposition::Never
        },
    )
}

fn ensure_file_adapter_usable(adapter: &dyn RemoteFileAdapter) -> Result<(), RemoteRuntimeError> {
    match adapter.availability() {
        AdapterAvailability::Healthy | AdapterAvailability::Degraded(_) => {}
        AdapterAvailability::Unsupported(reason) => {
            return Err(RemoteRuntimeError::new(
                "remote_unsupported",
                reason.as_str(),
                false,
            ));
        }
        AdapterAvailability::Unreachable(reason) => {
            return Err(RemoteRuntimeError::new(
                "remote_unreachable",
                reason.as_str(),
                true,
            ));
        }
    }
    if !adapter
        .capabilities()
        .iter()
        .any(|operation| operation.status.is_supported())
    {
        return Err(RemoteRuntimeError::new(
            "remote_unsupported",
            "remote_file_operations_unsupported",
            false,
        ));
    }
    Ok(())
}

fn transfer_protocol_unsupported(protocol: RemoteProtocol) -> RemoteRuntimeError {
    let reason = match protocol {
        RemoteProtocol::Ssh => "ssh_terminal_only",
        RemoteProtocol::Sftp
        | RemoteProtocol::Ftp
        | RemoteProtocol::FtpsExplicit
        | RemoteProtocol::Smb => "remote_protocol_has_no_file_adapter",
    };
    RemoteRuntimeError::new("transfer_unsupported", reason, false)
}

fn map_transfer_executor_error(error: ExecutorError) -> RemoteRuntimeError {
    match error {
        ExecutorError::QueuePoisoned => {
            RemoteRuntimeError::new("transfer_unavailable", "transfer_queue_poisoned", true)
        }
        ExecutorError::TaskDisappeared => {
            RemoteRuntimeError::new("transfer_unavailable", "transfer_task_disappeared", true)
        }
        ExecutorError::ExecutorClosed => RemoteRuntimeError::new(
            "transfer_provider_unavailable",
            "transfer_runner_stopped",
            true,
        ),
        ExecutorError::WorkerFailed => {
            RemoteRuntimeError::new("transfer_unavailable", "transfer_worker_failed", true)
        }
        ExecutorError::PauseUnsupported => {
            RemoteRuntimeError::new("transfer_unsupported", "transfer_pause_unsupported", false)
        }
        ExecutorError::Queue(error) => map_transfer_queue_error(error),
        ExecutorError::Remote(error) => {
            let mapped = map_remote_error(error);
            RemoteRuntimeError::new(
                "transfer_local_handle_unavailable",
                mapped.reason,
                mapped.retryable,
            )
        }
    }
}

fn map_transfer_queue_error(error: QueueError) -> RemoteRuntimeError {
    match error {
        QueueError::QueueFull => {
            RemoteRuntimeError::new("transfer_capacity_exceeded", "transfer_queue_full", true)
        }
        QueueError::TaskNotFound => {
            RemoteRuntimeError::new("transfer_not_found", "transfer_task_not_found", false)
        }
        QueueError::DuplicateTask(_) => {
            RemoteRuntimeError::new("transfer_conflict", "transfer_task_already_exists", false)
        }
        QueueError::Mutation(error) => {
            let (code, reason) = match error {
                localdesk_transfers::TransferMutationError::InvalidState { .. } => (
                    "transfer_invalid_state",
                    "transfer_state_does_not_allow_operation",
                ),
                localdesk_transfers::TransferMutationError::UnsupportedFeature(_) => {
                    ("transfer_unsupported", "transfer_feature_unsupported")
                }
                localdesk_transfers::TransferMutationError::NotRetryable => {
                    ("transfer_not_retryable", "transfer_failure_not_retryable")
                }
                localdesk_transfers::TransferMutationError::RetryExhausted => (
                    "transfer_retry_exhausted",
                    "transfer_retry_attempts_exhausted",
                ),
                localdesk_transfers::TransferMutationError::RetryNotReady => (
                    "transfer_retry_not_ready",
                    "transfer_retry_delay_not_elapsed",
                ),
                _ => ("transfer_invalid", "transfer_mutation_invalid"),
            };
            RemoteRuntimeError::new(code, reason, false)
        }
        QueueError::Store(error) => map_transfer_store_error(error),
        QueueError::InvalidPublicContract(_) => RemoteRuntimeError::new(
            "transfer_invalid",
            "transfer_public_contract_invalid",
            false,
        ),
    }
}

fn map_transfer_store_error(error: StoreError) -> RemoteRuntimeError {
    match error {
        StoreError::AlreadyExists | StoreError::RevisionConflict => {
            RemoteRuntimeError::new("transfer_conflict", "transfer_store_conflict", false)
        }
        StoreError::NotFound => {
            RemoteRuntimeError::new("transfer_not_found", "transfer_task_not_found", false)
        }
        StoreError::DocumentTooLarge => RemoteRuntimeError::new(
            "transfer_invalid",
            "transfer_task_document_too_large",
            false,
        ),
        StoreError::Unavailable => {
            RemoteRuntimeError::new("transfer_unavailable", "transfer_store_unavailable", true)
        }
        StoreError::Corrupt | StoreError::UnsupportedSchema => {
            RemoteRuntimeError::new("transfer_unavailable", "transfer_store_invalid", false)
        }
    }
}

fn session_snapshot_is_usable(snapshot: &localdesk_remote_core::RemoteSession) -> bool {
    matches!(
        snapshot.state,
        ConnectionState::Ready | ConnectionState::Degraded { .. }
    ) && snapshot
        .capabilities
        .iter()
        .any(|operation| operation.status.is_supported())
}

fn terminal_profile_error(reason_value: &'static str) -> RemoteError {
    RemoteError::new(
        RemoteErrorKind::NotFound,
        RemoteOperation::Connect,
        reason(reason_value),
        RetryDisposition::UserAction,
    )
}

fn terminal_session_not_found() -> RemoteRuntimeError {
    RemoteRuntimeError::new(
        "terminal_session_not_found",
        "terminal_session_not_found",
        false,
    )
}

fn remote_runtime_shutting_down() -> RemoteRuntimeError {
    RemoteRuntimeError::new(
        "remote_session_unavailable",
        "remote_runtime_shutting_down",
        false,
    )
}

fn map_terminal_capabilities() -> PublicTerminalCapabilities {
    PublicTerminalCapabilities {
        max_output_chunk_bytes: u32::try_from(
            TERMINAL_CAPABILITIES
                .max_output_chunk_bytes
                .min(MAX_TERMINAL_IPC_BYTES),
        )
        .expect("terminal output bound fits u32"),
        max_input_chunk_bytes: u32::try_from(
            TERMINAL_CAPABILITIES
                .max_input_chunk_bytes
                .min(MAX_TERMINAL_IPC_BYTES),
        )
        .expect("terminal input bound fits u32"),
        max_transcript_bytes: u32::try_from(TERMINAL_CAPABILITIES.max_transcript_bytes)
            .expect("terminal transcript bound fits u32"),
        max_rows: TERMINAL_CAPABILITIES.max_rows,
        max_columns: TERMINAL_CAPABILITIES.max_columns,
        max_pixel_dimension: TERMINAL_CAPABILITIES.max_pixel_dimension,
        nonblocking_output: TERMINAL_CAPABILITIES.nonblocking_output,
        fixed_openssh_program: TERMINAL_CAPABILITIES.fixed_openssh_program,
    }
}

fn map_terminal_status(
    status: SshTerminalStatus,
) -> Result<PublicTerminalStatus, RemoteRuntimeError> {
    let state = match status.state {
        SessionState::Running => PublicTerminalState::Running,
        SessionState::Exited { code } => PublicTerminalState::Exited { code },
        SessionState::Disconnected { reason } => PublicTerminalState::Disconnected {
            reason: match reason {
                DisconnectReason::HostKeyChanged => PublicDisconnectReason::HostKeyChanged,
                DisconnectReason::HostKeyRevoked => PublicDisconnectReason::HostKeyRevoked,
                DisconnectReason::HostKeyUnknown => PublicDisconnectReason::HostKeyUnknown,
                DisconnectReason::AuthenticationFailed => {
                    PublicDisconnectReason::AuthenticationFailed
                }
                DisconnectReason::NetworkUnreachable => PublicDisconnectReason::NetworkUnreachable,
                DisconnectReason::ConnectionLost => PublicDisconnectReason::ConnectionLost,
                DisconnectReason::OpenSshFailure => PublicDisconnectReason::OpenSshFailure,
            },
        },
        SessionState::ClosedByClient => PublicTerminalState::ClosedByClient,
    };
    let status = PublicTerminalStatus {
        state,
        transcript_retained_bytes: u32::try_from(status.transcript_retained_bytes).map_err(
            |_| {
                RemoteRuntimeError::new(
                    "terminal_response_invalid",
                    "terminal_transcript_size_invalid",
                    false,
                )
            },
        )?,
        transcript_dropped_bytes: status.transcript_dropped_bytes,
    };
    status.validate().map_err(|_| {
        RemoteRuntimeError::new(
            "terminal_response_invalid",
            "terminal_status_invalid",
            false,
        )
    })?;
    Ok(status)
}

fn remote_worker_failed() -> RemoteRuntimeError {
    RemoteRuntimeError::new("remote_worker_failed", "remote_worker_failed", true)
}

#[derive(Clone)]
struct SecretToolStore {
    program: PathBuf,
}

impl SecretToolStore {
    fn system() -> Self {
        Self {
            program: PathBuf::from(SECRET_TOOL_PROGRAM),
        }
    }

    async fn resolve_reference(
        &self,
        reference: &SecretRef,
    ) -> Result<SecretValue, SecretStoreError> {
        if reference.backend() != SecretBackend::SecretService || !is_executable(&self.program) {
            return Err(SecretStoreError::Unavailable(reason(
                "secret_service_unavailable",
            )));
        }
        let item_id = reference.item_id().to_string();
        let mut command = secret_tool_command(&self.program);
        command
            .args(["lookup", "localdesk-item-id", item_id.as_str()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        let mut child = spawn_secret_tool(&mut command)
            .map_err(|_| SecretStoreError::Unavailable(reason("secret_service_unavailable")))?;
        let stdout = child.stdout.take().ok_or_else(|| {
            SecretStoreError::Backend(reason("secret_service_output_unavailable"))
        })?;
        let mut bytes = Vec::new();
        stdout
            .take(u64::try_from(MAX_SECRET_INPUT_BYTES + 2).expect("bounded secret limit"))
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| SecretStoreError::Backend(reason("secret_service_read_failed")))?;
        wait_secret_tool(&mut child)
            .await
            .map_err(|_| SecretStoreError::Backend(reason("secret_service_lookup_failed")))?;
        if bytes.last() == Some(&b'\n') {
            bytes.pop();
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
        }
        if bytes.is_empty() {
            return Err(SecretStoreError::NotFound(reason("secret_not_found")));
        }
        if bytes.len() > MAX_SECRET_INPUT_BYTES {
            bytes.fill(0);
            return Err(SecretStoreError::Backend(reason(
                "secret_service_value_too_large",
            )));
        }
        Ok(SecretValue::new(bytes))
    }

    async fn delete_reference(&self, reference: &SecretRef) -> Result<(), SecretStoreError> {
        if reference.backend() != SecretBackend::SecretService || !is_executable(&self.program) {
            return Err(SecretStoreError::Unavailable(reason(
                "secret_service_unavailable",
            )));
        }
        let item_id = reference.item_id().to_string();
        let mut command = secret_tool_command(&self.program);
        command
            .args(["clear", "localdesk-item-id", item_id.as_str()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut child = spawn_secret_tool(&mut command)
            .map_err(|_| SecretStoreError::Unavailable(reason("secret_service_unavailable")))?;
        wait_secret_tool(&mut child)
            .await
            .map_err(|_| SecretStoreError::Backend(reason("secret_service_delete_failed")))
    }
}

impl SecretStore for SecretToolStore {
    fn resolve<'a>(
        &'a self,
        reference: &'a SecretRef,
    ) -> AdapterFuture<'a, Result<SecretValue, SecretStoreError>> {
        Box::pin(async move { self.resolve_reference(reference).await })
    }

    fn delete<'a>(
        &'a self,
        reference: &'a SecretRef,
    ) -> AdapterFuture<'a, Result<(), SecretStoreError>> {
        Box::pin(async move { self.delete_reference(reference).await })
    }
}

fn secret_tool_command(program: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .kill_on_drop(true)
        .env_remove("PASSWD")
        .env_remove("PASSWD_FD")
        .env_remove("PASSWD_FILE");
    command
}

fn spawn_secret_tool(command: &mut Command) -> std::io::Result<Child> {
    const ATTEMPTS: usize = 20;
    for attempt in 0..ATTEMPTS {
        match command.spawn() {
            Err(error) if transient_secret_spawn_error(&error) && attempt + 1 < ATTEMPTS => {
                std::thread::sleep(Duration::from_millis(10));
            }
            result => return result,
        }
    }
    unreachable!("bounded spawn retry loop always returns on its final attempt")
}

fn transient_secret_spawn_error(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
    ) || error.raw_os_error() == Some(26)
}

async fn wait_secret_tool(child: &mut Child) -> Result<(), RemoteRuntimeError> {
    match timeout(SECRET_TOOL_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(_)) => Err(RemoteRuntimeError::new(
            "secret_store_failed",
            "secret_service_operation_failed",
            true,
        )),
        Ok(Err(_)) => Err(secret_store_unavailable()),
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            Err(RemoteRuntimeError::new(
                "secret_store_timeout",
                "secret_service_operation_timed_out",
                true,
            ))
        }
    }
}

const fn secret_kind_code(kind: SecretKind) -> &'static str {
    match kind {
        SecretKind::Password => "password",
        SecretKind::PrivateKey => "private_key",
        SecretKind::KeyPassphrase => "key_passphrase",
    }
}

fn secret_input_invalid() -> RemoteRuntimeError {
    RemoteRuntimeError::new("secret_input_invalid", "secret_input_invalid", false)
}

fn secret_store_unavailable() -> RemoteRuntimeError {
    RemoteRuntimeError::new(
        "secret_store_unavailable",
        "secret_service_unavailable",
        true,
    )
}

struct RemoteProfileStore {
    connection: Connection,
}

impl RemoteProfileStore {
    fn open(path: &Path) -> Result<Self, RemoteRuntimeError> {
        let mut connection = Connection::open(path).map_err(|_| profile_store_unavailable())?;
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|_| profile_store_unavailable())?;
        ensure_profile_database_integrity(&connection)?;
        let version: u32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|_| profile_store_unavailable())?;
        if version > REMOTE_PROFILE_SCHEMA_VERSION {
            return Err(RemoteRuntimeError::new(
                "remote_profile_unavailable",
                "remote_profile_schema_unsupported",
                false,
            ));
        }
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;\nPRAGMA journal_mode = WAL;\nPRAGMA synchronous = FULL;",
            )
            .map_err(|_| profile_store_unavailable())?;
        match version {
            0 => {
                let transaction = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(|_| profile_store_unavailable())?;
                transaction
                    .execute_batch(REMOTE_PROFILE_SCHEMA_V1)
                    .map_err(|_| profile_store_unavailable())?;
                transaction
                    .commit()
                    .map_err(|_| profile_store_unavailable())?;
            }
            REMOTE_PROFILE_SCHEMA_VERSION => {}
            _ => {
                return Err(RemoteRuntimeError::new(
                    "remote_profile_unavailable",
                    "remote_profile_schema_unsupported",
                    false,
                ));
            }
        }
        Ok(Self { connection })
    }

    fn execute(
        &mut self,
        command: RemoteProfileCommand,
        transfer_referenced: bool,
    ) -> Result<RemoteProfileResult, RemoteRuntimeError> {
        match command {
            RemoteProfileCommand::List { query } => {
                let limit = usize::from(query.limit);
                let after = query
                    .after
                    .map_or_else(String::new, |id| id.as_uuid().to_string());
                let mut statement = self
                    .connection
                    .prepare(
                        "SELECT profile_id, protocol, revision, document, \
                         created_at_unix_ms, updated_at_unix_ms \
                         FROM remote_profiles WHERE profile_id > ?1 \
                         ORDER BY profile_id ASC LIMIT ?2",
                    )
                    .map_err(|_| profile_store_unavailable())?;
                let mut rows = statement
                    .query(params![after, i64::from(query.limit) + 1])
                    .map_err(|_| profile_store_unavailable())?;
                let mut profiles = Vec::new();
                while let Some(row) = rows.next().map_err(|_| profile_store_unavailable())? {
                    profiles.push(decode_profile_row(
                        row.get(0).map_err(|_| profile_store_corrupt())?,
                        row.get(1).map_err(|_| profile_store_corrupt())?,
                        row.get(2).map_err(|_| profile_store_corrupt())?,
                        row.get(3).map_err(|_| profile_store_corrupt())?,
                        row.get(4).map_err(|_| profile_store_corrupt())?,
                        row.get(5).map_err(|_| profile_store_corrupt())?,
                    )?);
                }
                let has_more = profiles.len() > limit;
                profiles.truncate(limit);
                let next_after = has_more.then(|| {
                    profiles
                        .last()
                        .expect("positive validated page limit")
                        .profile
                        .id
                });
                let page = RemoteProfilePage {
                    profiles,
                    next_after,
                };
                page.validate(query).map_err(|_| profile_store_corrupt())?;
                Ok(RemoteProfileResult::Page(page))
            }
            RemoteProfileCommand::Upsert {
                profile,
                expected_revision,
            } => self.upsert(profile, expected_revision, transfer_referenced),
            RemoteProfileCommand::Delete {
                profile_id,
                expected_revision,
            } => self.delete(profile_id, expected_revision, transfer_referenced),
        }
    }

    fn upsert(
        &mut self,
        profile: RemoteConnectionProfile,
        expected_revision: Option<u64>,
        transfer_referenced: bool,
    ) -> Result<RemoteProfileResult, RemoteRuntimeError> {
        profile.validate().map_err(|_| remote_profile_invalid())?;
        let now = unix_time_ms();
        let document = encode_profile(&profile)?;
        let existing = load_profile_by_id(&self.connection, profile.id)?;
        if let Some(current) = &existing {
            if expected_revision != Some(current.revision) {
                return Err(remote_profile_conflict());
            }
            if current.profile.protocol != profile.protocol
                && profile_is_referenced(&self.connection, profile.id, transfer_referenced)?
            {
                return Err(RemoteRuntimeError::new(
                    "remote_profile_in_use",
                    "remote_profile_protocol_change_in_use",
                    false,
                ));
            }
        } else if expected_revision.is_some() {
            return Err(RemoteRuntimeError::new(
                "remote_profile_not_found",
                "remote_profile_not_found",
                false,
            ));
        }
        let revision = expected_revision.map_or(0, |value| value + 1);
        let created_at_unix_ms = existing
            .as_ref()
            .map_or(now, |stored| stored.created_at_unix_ms);
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| profile_store_unavailable())?;
        let changed = if let Some(expected) = expected_revision {
            transaction
                .execute(
                    "UPDATE remote_profiles SET protocol = ?1, revision = ?2, document = ?3, \
                     updated_at_unix_ms = ?4 WHERE profile_id = ?5 AND revision = ?6",
                    params![
                        protocol_code(profile.protocol),
                        i64::try_from(revision).map_err(|_| remote_profile_invalid())?,
                        document,
                        now,
                        profile.id.as_uuid().to_string(),
                        i64::try_from(expected).map_err(|_| remote_profile_invalid())?,
                    ],
                )
                .map_err(|_| profile_store_unavailable())?
        } else {
            transaction
                .execute(
                    "INSERT OR IGNORE INTO remote_profiles \
                     (profile_id, protocol, revision, document, created_at_unix_ms, updated_at_unix_ms) \
                     VALUES (?1, ?2, 0, ?3, ?4, ?4)",
                    params![
                        profile.id.as_uuid().to_string(),
                        protocol_code(profile.protocol),
                        document,
                        now,
                    ],
                )
                .map_err(|_| profile_store_unavailable())?
        };
        if changed != 1 {
            return Err(remote_profile_conflict());
        }
        transaction
            .commit()
            .map_err(|_| profile_store_unavailable())?;
        Ok(RemoteProfileResult::Stored(StoredRemoteProfile {
            profile,
            revision,
            created_at_unix_ms,
            updated_at_unix_ms: now,
        }))
    }

    fn delete(
        &mut self,
        profile_id: ProfileId,
        expected_revision: u64,
        transfer_referenced: bool,
    ) -> Result<RemoteProfileResult, RemoteRuntimeError> {
        if profile_is_referenced(&self.connection, profile_id, transfer_referenced)? {
            return Err(RemoteRuntimeError::new(
                "remote_profile_in_use",
                "remote_profile_in_use",
                false,
            ));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| profile_store_unavailable())?;
        let changed = transaction
            .execute(
                "DELETE FROM remote_profiles WHERE profile_id = ?1 AND revision = ?2",
                params![
                    profile_id.as_uuid().to_string(),
                    i64::try_from(expected_revision).map_err(|_| remote_profile_invalid())?,
                ],
            )
            .map_err(|_| profile_store_unavailable())?;
        if changed != 1 {
            let exists: bool = transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM remote_profiles WHERE profile_id = ?1)",
                    [profile_id.as_uuid().to_string()],
                    |row| row.get(0),
                )
                .map_err(|_| profile_store_unavailable())?;
            return Err(if exists {
                remote_profile_conflict()
            } else {
                RemoteRuntimeError::new(
                    "remote_profile_not_found",
                    "remote_profile_not_found",
                    false,
                )
            });
        }
        transaction
            .commit()
            .map_err(|_| profile_store_unavailable())?;
        Ok(RemoteProfileResult::Deleted { profile_id })
    }
}

fn ensure_profile_database_integrity(connection: &Connection) -> Result<(), RemoteRuntimeError> {
    let result = connection
        .query_row("PRAGMA quick_check(1)", [], |row| row.get::<_, String>(0))
        .map_err(|_| profile_store_corrupt())?;
    if result != "ok" {
        return Err(profile_store_corrupt());
    }
    Ok(())
}

fn load_profile_by_id(
    connection: &Connection,
    profile_id: ProfileId,
) -> Result<Option<StoredRemoteProfile>, RemoteRuntimeError> {
    connection
        .query_row(
            "SELECT profile_id, protocol, revision, document, created_at_unix_ms, \
             updated_at_unix_ms FROM remote_profiles WHERE profile_id = ?1",
            [profile_id.as_uuid().to_string()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()
        .map_err(|_| profile_store_unavailable())?
        .map(|(id, protocol, revision, document, created, updated)| {
            decode_profile_row(id, protocol, revision, document, created, updated)
        })
        .transpose()
}

fn encode_profile(profile: &RemoteConnectionProfile) -> Result<Vec<u8>, RemoteRuntimeError> {
    let document = serde_json::to_vec(profile).map_err(|_| remote_profile_invalid())?;
    if document.len() > MAX_REMOTE_PROFILE_DOCUMENT_BYTES {
        return Err(RemoteRuntimeError::new(
            "remote_profile_invalid",
            "remote_profile_document_too_large",
            false,
        ));
    }
    Ok(document)
}

fn decode_profile_row(
    profile_id: String,
    protocol: String,
    revision: i64,
    document: Vec<u8>,
    created_at_unix_ms: i64,
    updated_at_unix_ms: i64,
) -> Result<StoredRemoteProfile, RemoteRuntimeError> {
    if document.len() > MAX_REMOTE_PROFILE_DOCUMENT_BYTES || revision < 0 {
        return Err(profile_store_corrupt());
    }
    let profile: RemoteConnectionProfile =
        serde_json::from_slice(&document).map_err(|_| profile_store_corrupt())?;
    let stored = StoredRemoteProfile {
        profile,
        revision: u64::try_from(revision).map_err(|_| profile_store_corrupt())?,
        created_at_unix_ms,
        updated_at_unix_ms,
    };
    stored.validate().map_err(|_| profile_store_corrupt())?;
    if stored.profile.id.as_uuid().to_string() != profile_id
        || protocol_code(stored.profile.protocol) != protocol
    {
        return Err(profile_store_corrupt());
    }
    Ok(stored)
}

fn profile_is_referenced(
    connection: &Connection,
    profile_id: ProfileId,
    transfer_referenced: bool,
) -> Result<bool, RemoteRuntimeError> {
    let mut statement = connection
        .prepare(
            "SELECT profile_id, protocol, revision, document, created_at_unix_ms, \
             updated_at_unix_ms FROM remote_profiles",
        )
        .map_err(|_| profile_store_unavailable())?;
    let mut rows = statement
        .query([])
        .map_err(|_| profile_store_unavailable())?;
    while let Some(row) = rows.next().map_err(|_| profile_store_unavailable())? {
        let stored = decode_profile_row(
            row.get(0).map_err(|_| profile_store_corrupt())?,
            row.get(1).map_err(|_| profile_store_corrupt())?,
            row.get(2).map_err(|_| profile_store_corrupt())?,
            row.get(3).map_err(|_| profile_store_corrupt())?,
            row.get(4).map_err(|_| profile_store_corrupt())?,
            row.get(5).map_err(|_| profile_store_corrupt())?,
        )?;
        let jumps = match stored.profile.options {
            ProfileOptions::Ssh { jump_profiles, .. } | ProfileOptions::Sftp { jump_profiles } => {
                jump_profiles
            }
            _ => Vec::new(),
        };
        if stored.profile.id != profile_id && jumps.contains(&profile_id) {
            return Ok(true);
        }
    }
    Ok(transfer_referenced)
}

fn load_transfer_profile_references(
    transfer_database: &Path,
) -> Result<HashSet<ProfileId>, StoreError> {
    Ok(SqliteTransferStore::open(transfer_database)?
        .load_all()?
        .into_iter()
        .map(|task| task.remote_profile_id())
        .collect())
}

fn protocol_code(protocol: RemoteProtocol) -> &'static str {
    match protocol {
        RemoteProtocol::Ssh => "ssh",
        RemoteProtocol::Sftp => "sftp",
        RemoteProtocol::Ftp => "ftp",
        RemoteProtocol::FtpsExplicit => "ftps_explicit",
        RemoteProtocol::Smb => "smb",
    }
}

fn profile_store_unavailable() -> RemoteRuntimeError {
    RemoteRuntimeError::new(
        "remote_profile_unavailable",
        "remote_profile_store_unavailable",
        true,
    )
}

fn profile_store_corrupt() -> RemoteRuntimeError {
    RemoteRuntimeError::new(
        "remote_profile_unavailable",
        "remote_profile_store_corrupt",
        false,
    )
}

fn remote_profile_invalid() -> RemoteRuntimeError {
    RemoteRuntimeError::new("remote_profile_invalid", "remote_profile_invalid", false)
}

fn remote_profile_conflict() -> RemoteRuntimeError {
    RemoteRuntimeError::new(
        "remote_profile_conflict",
        "remote_profile_revision_conflict",
        false,
    )
}

fn descriptor_from_file_adapter(
    adapter: &dyn RemoteFileAdapter,
    terminal_reason: &'static str,
) -> RemoteAdapterDescriptor {
    RemoteAdapterDescriptor {
        protocol: adapter.protocol(),
        availability: adapter.availability(),
        terminal: CapabilityStatus::Unsupported(reason(terminal_reason)),
        file_operations: adapter.capabilities().clone(),
    }
}

fn unsupported_descriptor(protocol: RemoteProtocol, reason_code: &str) -> RemoteAdapterDescriptor {
    let unavailable = reason(reason_code);
    RemoteAdapterDescriptor {
        protocol,
        availability: AdapterAvailability::Unsupported(unavailable.clone()),
        terminal: CapabilityStatus::Unsupported(unavailable.clone()),
        file_operations: unsupported_file_capabilities(unavailable),
    }
}

fn map_availability(availability: &AdapterAvailability) -> CapabilityRuntimeState {
    match availability {
        AdapterAvailability::Healthy => CapabilityRuntimeState::healthy("remote_adapter_available"),
        AdapterAvailability::Degraded(reason) => CapabilityRuntimeState::degraded(reason.as_str()),
        AdapterAvailability::Unsupported(reason) => {
            CapabilityRuntimeState::unsupported(reason.as_str())
        }
        AdapterAvailability::Unreachable(reason) => {
            CapabilityRuntimeState::unreachable(reason.as_str())
        }
    }
}

fn state_base_from_environment() -> Result<PathBuf, RemoteRuntimeError> {
    let base = if let Some(path) = std::env::var_os("XDG_STATE_HOME") {
        PathBuf::from(path)
    } else {
        let home = std::env::var_os("HOME").ok_or_else(|| {
            RemoteRuntimeError::new(
                "remote_state_unavailable",
                "home_directory_unavailable",
                false,
            )
        })?;
        PathBuf::from(home).join(".local/state")
    };
    if !base.is_absolute() {
        return Err(RemoteRuntimeError::new(
            "remote_state_unsafe",
            "remote_state_path_not_absolute",
            false,
        ));
    }
    Ok(base)
}

fn prepare_private_state(
    base: &Path,
    expected_uid: u32,
) -> Result<(PathBuf, PathBuf, PathBuf), RemoteRuntimeError> {
    validate_private_directory(base, expected_uid)?;
    let directory = base.join(STATE_DIRECTORY);
    if !directory.exists() {
        DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(|_| {
                RemoteRuntimeError::new(
                    "remote_state_unavailable",
                    "remote_state_directory_create_failed",
                    true,
                )
            })?;
    }
    validate_private_directory(&directory, expected_uid)?;
    let known_hosts = prepare_private_file(&directory.join(KNOWN_HOSTS_FILE), expected_uid)?;
    let transfer_database =
        prepare_private_file(&directory.join(TRANSFER_DATABASE_FILE), expected_uid)?;
    let remote_database =
        prepare_private_file(&directory.join(REMOTE_DATABASE_FILE), expected_uid)?;
    Ok((known_hosts, transfer_database, remote_database))
}

fn validate_private_directory(path: &Path, expected_uid: u32) -> Result<(), RemoteRuntimeError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| {
        RemoteRuntimeError::new(
            "remote_state_unavailable",
            "remote_state_directory_unavailable",
            true,
        )
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.file_type().is_dir()
        || metadata.uid() != expected_uid
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(RemoteRuntimeError::new(
            "remote_state_unsafe",
            "remote_state_directory_unsafe",
            false,
        ));
    }
    Ok(())
}

fn prepare_private_file(path: &Path, expected_uid: u32) -> Result<PathBuf, RemoteRuntimeError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink()
                || !metadata.file_type().is_file()
                || metadata.uid() != expected_uid
                || metadata.permissions().mode() & 0o777 != 0o600
            {
                return Err(RemoteRuntimeError::new(
                    "remote_state_unsafe",
                    "remote_state_file_unsafe",
                    false,
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
                .map_err(|_| {
                    RemoteRuntimeError::new(
                        "remote_state_unavailable",
                        "remote_state_file_create_failed",
                        true,
                    )
                })?;
        }
        Err(_) => {
            return Err(RemoteRuntimeError::new(
                "remote_state_unavailable",
                "remote_state_file_unavailable",
                true,
            ));
        }
    }
    Ok(path.to_path_buf())
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

fn local_handle_display_metadata(
    path: &Path,
    purpose: TransferLocalHandlePurpose,
) -> Result<(String, Option<u64>), RemoteRuntimeError> {
    let display_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| {
            !name.is_empty()
                && name.len() <= localdesk_transfers::MAX_LOCAL_HANDLE_DISPLAY_NAME_BYTES
                && !name.chars().any(char::is_control)
                && !name.contains(['/', '\\'])
        })
        .ok_or_else(|| {
            RemoteRuntimeError::new(
                "transfer_local_handle_invalid",
                "transfer_local_handle_display_name_invalid",
                false,
            )
        })?
        .to_owned();
    let size_bytes = match purpose {
        TransferLocalHandlePurpose::UploadSource => Some(
            fs::symlink_metadata(path)
                .map_err(|_| {
                    RemoteRuntimeError::new(
                        "transfer_local_handle_unavailable",
                        "local_source_metadata_unavailable",
                        true,
                    )
                })?
                .len(),
        ),
        TransferLocalHandlePurpose::DownloadDestination => None,
    };
    Ok((display_name, size_bytes))
}

fn reason(value: &str) -> SafeReason {
    SafeReason::new(value).expect("static remote runtime reason is valid")
}

fn unix_time_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use localdesk_remote_core::{
        CapabilityMatrix, FILE_OPERATIONS, OperationCapability, SecretInput, SecretKind,
    };
    use std::sync::Condvar;

    static SECRET_TOOL_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct BlockingGate {
        released: Mutex<bool>,
        changed: Condvar,
    }

    impl BlockingGate {
        fn new() -> Self {
            Self {
                released: Mutex::new(false),
                changed: Condvar::new(),
            }
        }

        fn wait(&self) {
            let released = self
                .released
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let _released = self
                .changed
                .wait_while(released, |released| !*released)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }

        fn release(&self) {
            *self
                .released
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
            self.changed.notify_all();
        }
    }

    struct FakeFileSession {
        id: SessionId,
        disconnected: Arc<AtomicUsize>,
    }

    impl FakeFileSession {
        fn new(disconnected: Arc<AtomicUsize>) -> Self {
            Self {
                id: SessionId::new(),
                disconnected,
            }
        }

        fn unsupported<'a, T: Send + 'a>(
            operation: RemoteOperation,
        ) -> AdapterFuture<'a, Result<T, RemoteError>> {
            Box::pin(async move {
                Err(RemoteError::new(
                    RemoteErrorKind::Unsupported,
                    operation,
                    reason("fixture_operation_unsupported"),
                    RetryDisposition::Never,
                ))
            })
        }
    }

    impl RemoteFileSession for FakeFileSession {
        fn id(&self) -> SessionId {
            self.id
        }

        fn snapshot(&self) -> localdesk_remote_core::RemoteSession {
            let capabilities =
                CapabilityMatrix::complete(FILE_OPERATIONS.iter().map(|operation| {
                    OperationCapability {
                        operation: *operation,
                        status: if *operation == localdesk_remote_core::FileOperation::List {
                            CapabilityStatus::Supported
                        } else {
                            CapabilityStatus::Unsupported(reason("fixture_operation_unsupported"))
                        },
                    }
                }))
                .expect("complete capabilities");
            localdesk_remote_core::RemoteSession {
                id: self.id,
                profile_id: ProfileId::new(),
                protocol: RemoteProtocol::Sftp,
                state: ConnectionState::Ready,
                capabilities,
                opened_at_unix_ms: 1,
                updated_at_unix_ms: 1,
            }
        }

        fn list<'a>(
            &'a self,
            _path: &'a localdesk_remote_core::RemotePath,
        ) -> AdapterFuture<'a, Result<Vec<RemoteEntry>, RemoteError>> {
            Self::unsupported(RemoteOperation::List)
        }

        fn stat<'a>(
            &'a self,
            _path: &'a localdesk_remote_core::RemotePath,
        ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>> {
            Self::unsupported(RemoteOperation::Stat)
        }

        fn create_directory<'a>(
            &'a self,
            _path: &'a localdesk_remote_core::RemotePath,
        ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>> {
            Self::unsupported(RemoteOperation::CreateDirectory)
        }

        fn rename<'a>(
            &'a self,
            _from: &'a localdesk_remote_core::RemotePath,
            _to: &'a localdesk_remote_core::RemotePath,
        ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>> {
            Self::unsupported(RemoteOperation::Rename)
        }

        fn delete<'a>(
            &'a self,
            _path: &'a localdesk_remote_core::RemotePath,
        ) -> AdapterFuture<'a, Result<(), RemoteError>> {
            Self::unsupported(RemoteOperation::Delete)
        }

        fn read_chunk<'a>(
            &'a self,
            _request: RemoteReadRequest,
        ) -> AdapterFuture<'a, Result<RemoteReadChunk, RemoteError>> {
            Self::unsupported(RemoteOperation::Read)
        }

        fn begin_write<'a>(
            &'a self,
            _request: BeginWriteRequest,
        ) -> AdapterFuture<'a, Result<RemoteWriteReceipt, RemoteError>> {
            Self::unsupported(RemoteOperation::Write)
        }

        fn write_chunk<'a>(
            &'a self,
            _handle: RemoteWriteHandle,
            _offset: u64,
            _bytes: Vec<u8>,
        ) -> AdapterFuture<'a, Result<RemoteWriteReceipt, RemoteError>> {
            Self::unsupported(RemoteOperation::Write)
        }

        fn commit_write<'a>(
            &'a self,
            _handle: RemoteWriteHandle,
            _expected_identity: Option<ObjectIdentity>,
        ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>> {
            Self::unsupported(RemoteOperation::Write)
        }

        fn abort_write<'a>(
            &'a self,
            _handle: RemoteWriteHandle,
        ) -> AdapterFuture<'a, Result<(), RemoteError>> {
            Self::unsupported(RemoteOperation::Write)
        }

        fn disconnect<'a>(&'a self) -> AdapterFuture<'a, Result<(), RemoteError>> {
            self.disconnected.fetch_add(1, Ordering::AcqRel);
            Box::pin(async { Ok(()) })
        }
    }

    struct FakeTerminalSession {
        closed: Arc<AtomicUsize>,
    }

    impl FakeTerminalSession {
        fn status() -> SshTerminalStatus {
            SshTerminalStatus {
                state: SessionState::ClosedByClient,
                pending_output_bytes: 0,
                pending_output_dropped_bytes: 0,
                transcript_retained_bytes: 0,
                transcript_dropped_bytes: 0,
            }
        }
    }

    impl TerminalRuntimeSession for FakeTerminalSession {
        fn read_output(&mut self, _max_bytes: usize) -> Result<SshTerminalRead, RemoteError> {
            Ok(SshTerminalRead::EndOfStream)
        }

        fn write_input(&mut self, _bytes: &[u8]) -> Result<(), RemoteError> {
            Ok(())
        }

        fn resize(&self, _size: PtySize) -> Result<(), RemoteError> {
            Ok(())
        }

        fn poll_state(&mut self) -> Result<SshTerminalStatus, RemoteError> {
            Ok(Self::status())
        }

        fn close(&mut self) -> Result<SshTerminalStatus, RemoteError> {
            self.closed.fetch_add(1, Ordering::AcqRel);
            Ok(Self::status())
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_blocking_connect_keeps_capacity_until_worker_exits() {
        let runtime = RemoteRuntime::unavailable_for_test("capacity_fixture");
        let held = (0..MAX_REMOTE_SESSIONS - 1)
            .map(|_| runtime.acquire_session_permit().expect("held permit"))
            .collect::<Vec<_>>();
        let permit = runtime.acquire_session_permit().expect("worker permit");
        let gate = Arc::new(BlockingGate::new());
        let worker_gate = Arc::clone(&gate);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let connect = tokio::spawn(async move {
            spawn_blocking_holding_permit(permit, move || {
                let _ = started_tx.send(());
                worker_gate.wait();
                Ok::<_, RemoteRuntimeError>(())
            })
            .await
        });
        started_rx.await.expect("blocking worker started");
        connect.abort();
        let _ = connect.await;
        assert_eq!(runtime.session_capacity.available_permits(), 0);
        assert!(runtime.acquire_session_permit().is_err());

        gate.release();
        timeout(Duration::from_secs(1), async {
            while runtime.session_capacity.available_permits() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("worker releases capacity");
        drop(held);
    }

    #[tokio::test]
    async fn sftp_session_future_stays_on_the_owner_runtime() {
        let owner = tokio::runtime::Handle::current().id();
        let observed = run_session_future(RemoteProtocol::Sftp, || {
            Box::pin(async { Ok::<_, RemoteError>(tokio::runtime::Handle::current().id()) })
        })
        .await
        .expect("SFTP future");
        assert_eq!(observed, owner);
    }

    #[tokio::test]
    async fn shutdown_rejects_and_closes_late_file_and_terminal_registrations() {
        let file_runtime = RemoteRuntime::unavailable_for_test("late_file_fixture");
        let disconnected = Arc::new(AtomicUsize::new(0));
        let file_gate = Arc::new(BlockingGate::new());
        let worker_gate = Arc::clone(&file_gate);
        let (file_started_tx, file_started_rx) = tokio::sync::oneshot::channel();
        let worker_runtime = file_runtime.clone();
        let worker_disconnected = Arc::clone(&disconnected);
        let file_connect = tokio::spawn(async move {
            let permit = worker_runtime
                .acquire_session_permit()
                .expect("pre-shutdown file permit");
            let (file, permit) = spawn_blocking_holding_permit(permit, move || {
                let _ = file_started_tx.send(());
                worker_gate.wait();
                Ok::<Arc<dyn RemoteFileSession>, RemoteRuntimeError>(Arc::new(
                    FakeFileSession::new(worker_disconnected),
                ))
            })
            .await
            .expect("file worker join")
            .expect("file connect");
            worker_runtime.register_file_session(file, permit).await
        });
        file_started_rx.await.expect("file worker started");
        file_runtime.close_session_admission().await;
        file_gate.release();
        let error = file_connect
            .await
            .expect("file connect task")
            .expect_err("late file session rejected");
        assert_eq!(error.reason, "remote_runtime_shutting_down");
        assert_eq!(disconnected.load(Ordering::Acquire), 1);
        assert!(file_runtime.sessions.lock().await.is_empty());

        let terminal_runtime = RemoteRuntime::unavailable_for_test("late_terminal_fixture");
        let closed = Arc::new(AtomicUsize::new(0));
        let terminal_gate = Arc::new(BlockingGate::new());
        let worker_gate = Arc::clone(&terminal_gate);
        let (terminal_started_tx, terminal_started_rx) = tokio::sync::oneshot::channel();
        let worker_runtime = terminal_runtime.clone();
        let worker_closed = Arc::clone(&closed);
        let terminal_open = tokio::spawn(async move {
            let permit = worker_runtime
                .acquire_session_permit()
                .expect("pre-shutdown terminal permit");
            let (terminal, permit) = spawn_blocking_holding_permit(permit, move || {
                let _ = terminal_started_tx.send(());
                worker_gate.wait();
                Ok::<Arc<Mutex<Box<dyn TerminalRuntimeSession>>>, RemoteRuntimeError>(Arc::new(
                    Mutex::new(Box::new(FakeTerminalSession {
                        closed: worker_closed,
                    })),
                ))
            })
            .await
            .expect("terminal worker join")
            .expect("terminal open");
            worker_runtime
                .register_terminal_session(terminal, permit)
                .await
        });
        terminal_started_rx.await.expect("terminal worker started");
        terminal_runtime.close_session_admission().await;
        terminal_gate.release();
        let error = terminal_open
            .await
            .expect("terminal open task")
            .expect_err("late terminal session rejected");
        assert_eq!(error.reason, "remote_runtime_shutting_down");
        assert_eq!(closed.load(Ordering::Acquire), 1);
        assert!(terminal_runtime.terminal_sessions.lock().await.is_empty());
    }

    #[tokio::test]
    async fn explicit_session_churn_cancels_reapers_promptly() {
        let runtime = RemoteRuntime::unavailable_for_test("reaper_fixture");
        for _ in 0..8 {
            let disconnected = Arc::new(AtomicUsize::new(0));
            let file: Arc<dyn RemoteFileSession> =
                Arc::new(FakeFileSession::new(Arc::clone(&disconnected)));
            let file_id = file.id();
            let permit = runtime.acquire_session_permit().expect("file permit");
            runtime
                .register_file_session(file, permit)
                .await
                .expect("register file");
            runtime
                .disconnect_file_session(file_id)
                .await
                .expect("disconnect file");
            assert_eq!(disconnected.load(Ordering::Acquire), 1);

            let closed = Arc::new(AtomicUsize::new(0));
            let terminal: Arc<Mutex<Box<dyn TerminalRuntimeSession>>> =
                Arc::new(Mutex::new(Box::new(FakeTerminalSession {
                    closed: Arc::clone(&closed),
                })));
            let permit = runtime.acquire_session_permit().expect("terminal permit");
            let terminal_id = runtime
                .register_terminal_session(terminal, permit)
                .await
                .expect("register terminal");
            runtime
                .close_terminal_session(terminal_id)
                .await
                .expect("close terminal");
            assert_eq!(closed.load(Ordering::Acquire), 1);
        }

        timeout(Duration::from_secs(1), async {
            while runtime.active_reapers.load(Ordering::Acquire) != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("reapers terminate after explicit close");
        assert!(runtime.sessions.lock().await.is_empty());
        assert!(runtime.terminal_sessions.lock().await.is_empty());
    }

    #[tokio::test]
    async fn terminal_background_reads_do_not_extend_idle_timeout() {
        let runtime = RemoteRuntime::unavailable_for_test("terminal_idle_fixture");
        let terminal: Arc<Mutex<Box<dyn TerminalRuntimeSession>>> =
            Arc::new(Mutex::new(Box::new(FakeTerminalSession {
                closed: Arc::new(AtomicUsize::new(0)),
            })));
        let permit = runtime.acquire_session_permit().expect("terminal permit");
        let terminal_id = runtime
            .register_terminal_session(terminal, permit)
            .await
            .expect("register terminal");
        let idle_at = Instant::now() - Duration::from_secs(60);
        runtime
            .terminal_sessions
            .lock()
            .await
            .get_mut(&terminal_id)
            .expect("registered terminal")
            .last_activity = idle_at;

        runtime
            .terminal_session(terminal_id)
            .await
            .expect("background terminal access");
        assert_eq!(
            runtime
                .terminal_sessions
                .lock()
                .await
                .get(&terminal_id)
                .expect("registered terminal")
                .last_activity,
            idle_at
        );

        runtime
            .active_terminal_session(terminal_id)
            .await
            .expect("interactive terminal access");
        assert!(
            runtime
                .terminal_sessions
                .lock()
                .await
                .get(&terminal_id)
                .expect("registered terminal")
                .last_activity
                > idle_at
        );
        runtime
            .close_terminal_session(terminal_id)
            .await
            .expect("close terminal");
    }

    #[test]
    fn shared_session_capacity_is_reserved_before_adapter_io() {
        let runtime = RemoteRuntime::unavailable_for_test("capacity_fixture");
        let mut permits = Vec::new();
        for _ in 0..MAX_REMOTE_SESSIONS {
            permits.push(runtime.acquire_session_permit().expect("capacity permit"));
        }
        let error = runtime
            .acquire_session_permit()
            .expect_err("capacity must be hard bounded");
        assert_eq!(error.code, "remote_session_busy");
        assert_eq!(error.reason, "remote_session_capacity_exceeded");
        assert!(error.retryable);

        drop(permits.pop());
        assert!(runtime.acquire_session_permit().is_ok());
    }

    #[test]
    fn session_expiry_enforces_both_idle_and_absolute_lease_deadlines() {
        let now = Instant::now();
        assert!(session_is_expired(now, now - REMOTE_SESSION_IDLE_TIMEOUT));
        assert!(session_is_expired(now - REMOTE_SESSION_MAX_LEASE, now));
        assert!(!session_is_expired(now, now));
        assert!(session_expiry_wait(now, now) > Duration::ZERO);
    }

    #[test]
    fn remote_error_kinds_have_stable_ipc_codes() {
        let cases = [
            (RemoteErrorKind::Transport, "remote_transport_error"),
            (RemoteErrorKind::Trust, "remote_trust_error"),
            (
                RemoteErrorKind::Authentication,
                "remote_authentication_error",
            ),
            (
                RemoteErrorKind::PermissionDenied,
                "remote_permission_denied",
            ),
            (RemoteErrorKind::NotFound, "remote_not_found"),
            (RemoteErrorKind::Conflict, "remote_conflict"),
            (RemoteErrorKind::Unsupported, "remote_unsupported"),
            (RemoteErrorKind::RateLimited, "remote_rate_limited"),
            (RemoteErrorKind::Timeout, "remote_timeout"),
            (RemoteErrorKind::RemoteProtocol, "remote_protocol_error"),
            (RemoteErrorKind::Cancelled, "remote_cancelled"),
            (RemoteErrorKind::InvalidInput, "remote_invalid_input"),
            (RemoteErrorKind::SecretStore, "remote_secret_store_error"),
        ];

        for (kind, expected) in cases {
            let mapped = map_remote_error(RemoteError::new(
                kind,
                RemoteOperation::Connect,
                reason("fixture_remote_error"),
                RetryDisposition::Never,
            ));
            assert_eq!(mapped.code, expected);
            assert_ne!(mapped.code, "remote_operation_failed");
        }
    }

    #[test]
    fn invalid_transfer_store_errors_are_non_retryable() {
        for error in [StoreError::Corrupt, StoreError::UnsupportedSchema] {
            let mapped = map_transfer_store_error(error);
            assert_eq!(mapped.code, "transfer_unavailable");
            assert_eq!(mapped.reason, "transfer_store_invalid");
            assert!(!mapped.retryable);
        }
    }

    #[test]
    fn runtime_catalog_is_complete_and_state_files_are_private() {
        let directory = tempfile::tempdir().expect("directory");
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))
            .expect("permissions");
        let runtime = RemoteRuntime::from_state_base(directory.path());
        let catalog = runtime.catalog();
        assert_eq!(catalog.validate(), Ok(()));
        assert_eq!(catalog.adapters.len(), 5);

        for file in [
            KNOWN_HOSTS_FILE,
            TRANSFER_DATABASE_FILE,
            REMOTE_DATABASE_FILE,
        ] {
            let metadata = fs::symlink_metadata(directory.path().join(STATE_DIRECTORY).join(file))
                .expect("metadata");
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn remote_profile_store_rejects_future_schema_without_modifying_it() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("remote.sqlite3");
        let connection = Connection::open(&path).expect("future database");
        connection
            .pragma_update(None, "user_version", 99_u32)
            .expect("future schema version");
        drop(connection);
        let before = fs::read(&path).expect("future database bytes");

        let error = match RemoteProfileStore::open(&path) {
            Ok(_) => panic!("future schema must be rejected"),
            Err(error) => error,
        };
        assert_eq!(error.code, "remote_profile_unavailable");
        assert_eq!(error.reason, "remote_profile_schema_unsupported");
        assert!(!error.retryable);
        assert_eq!(fs::read(&path).expect("database after rejection"), before);
        assert!(!path.with_extension("sqlite3-wal").exists());
        assert!(!path.with_extension("sqlite3-shm").exists());
    }

    #[test]
    fn failed_remote_profile_initialization_preserves_the_version_zero_database() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("remote.sqlite3");
        let connection = Connection::open(&path).expect("fixture database");
        connection
            .execute_batch(
                "CREATE TABLE remote_profiles (sentinel INTEGER NOT NULL) STRICT;
                 INSERT INTO remote_profiles (sentinel) VALUES (42);",
            )
            .expect("incompatible version zero fixture");
        drop(connection);

        let error = match RemoteProfileStore::open(&path) {
            Ok(_) => panic!("incompatible schema must fail initialization"),
            Err(error) => error,
        };
        assert_eq!(error.code, "remote_profile_unavailable");
        assert_eq!(error.reason, "remote_profile_store_unavailable");

        let connection = Connection::open(&path).expect("reopen fixture");
        let version: u32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("schema version");
        let sentinel: i64 = connection
            .query_row("SELECT sentinel FROM remote_profiles", [], |row| row.get(0))
            .expect("sentinel row");
        let index_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'index' AND name = 'remote_profiles_protocol_idx'",
                [],
                |row| row.get(0),
            )
            .expect("index count");
        assert_eq!(version, 0);
        assert_eq!(sentinel, 42);
        assert_eq!(index_count, 0);
    }

    #[test]
    fn corrupt_remote_profile_database_is_rejected_without_rebuild_or_sidecars() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("remote.sqlite3");
        let before = b"not a sqlite database";
        fs::write(&path, before).expect("corrupt fixture");

        let error = match RemoteProfileStore::open(&path) {
            Ok(_) => panic!("corrupt database must be rejected"),
            Err(error) => error,
        };
        assert_eq!(error.code, "remote_profile_unavailable");
        assert_eq!(error.reason, "remote_profile_store_corrupt");
        assert!(!error.retryable);
        assert_eq!(fs::read(&path).expect("fixture bytes"), before);
        assert!(!path.with_extension("sqlite3-wal").exists());
        assert!(!path.with_extension("sqlite3-shm").exists());
    }

    #[test]
    fn private_state_rejects_symlink_files() {
        let directory = tempfile::tempdir().expect("directory");
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))
            .expect("permissions");
        let state = directory.path().join(STATE_DIRECTORY);
        fs::create_dir(&state).expect("state");
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).expect("permissions");
        std::os::unix::fs::symlink("target", state.join(KNOWN_HOSTS_FILE)).expect("symlink");
        assert_eq!(
            prepare_private_state(directory.path(), Uid::current().as_raw())
                .expect_err("unsafe")
                .reason,
            "remote_state_file_unsafe"
        );
    }

    #[tokio::test]
    async fn secret_tool_receives_material_only_on_stdin_and_uses_fixed_attributes() {
        let _guard = SECRET_TOOL_TEST_LOCK.lock().await;
        let directory = tempfile::tempdir().expect("directory");
        let program = directory.path().join("secret-tool-fixture");
        fs::write(
            &program,
            "#!/bin/sh\nbase=${0%/*}\nprintf '%s\\n' \"$@\" > \"$base/argv\"\nIFS= read -r value || :\nprintf '%s' \"$value\" > \"$base/stdin\"\n",
        )
        .expect("script");
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).expect("permissions");
        let runtime = RemoteRuntime::without_private_state("fixture");
        let secret = b"fixture-super-secret";
        let result = runtime
            .secret_command_with_program(
                SecretCommand::Store {
                    kind: SecretKind::Password,
                    value: SecretInput::new(secret.to_vec()).expect("secret"),
                },
                &program,
            )
            .await
            .expect("stored");
        assert!(matches!(result, SecretCommandResult::Stored { .. }));

        let argv = fs::read(directory.path().join("argv")).expect("argv");
        let stdin = fs::read(directory.path().join("stdin")).expect("stdin");
        assert_eq!(stdin, secret);
        assert!(!argv.windows(secret.len()).any(|window| window == secret));
        let argv = String::from_utf8(argv).expect("utf8 argv");
        assert!(argv.contains("localdesk-item-id"));
        assert!(argv.contains("localdesk-secret-kind"));
        assert!(argv.contains("password"));
    }

    #[tokio::test]
    async fn secret_tool_lookup_is_bounded_and_redacted_with_fixed_attributes() {
        let _guard = SECRET_TOOL_TEST_LOCK.lock().await;
        let directory = tempfile::tempdir().expect("directory");
        let program = directory.path().join("secret-tool-lookup-fixture");
        fs::write(
            &program,
            "#!/bin/sh\nbase=${0%/*}\nprintf '%s\\n' \"$@\" > \"$base/argv\"\nprintf '%s' \"${PASSWD-unset}\" > \"$base/passwd-env\"\nprintf '%s\\n' 'lookup-super-secret'\n",
        )
        .expect("script");
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).expect("permissions");
        let store = SecretToolStore {
            program: program.clone(),
        };
        let reference = SecretRef::secret_service(Uuid::from_u128(900));

        let value = store.resolve_reference(&reference).await.expect("lookup");
        assert_eq!(value.expose_secret(), b"lookup-super-secret");
        assert_eq!(format!("{value:?}"), "SecretValue(<redacted>)");
        assert_eq!(
            fs::read_to_string(directory.path().join("passwd-env")).expect("environment"),
            "unset"
        );
        let argv = fs::read_to_string(directory.path().join("argv")).expect("argv");
        assert_eq!(
            argv,
            format!("lookup\nlocaldesk-item-id\n{}\n", reference.item_id())
        );
        assert!(!argv.contains("lookup-super-secret"));
    }

    #[tokio::test]
    async fn secret_tool_lookup_rejects_oversized_output_without_leaking_it() {
        let _guard = SECRET_TOOL_TEST_LOCK.lock().await;
        let directory = tempfile::tempdir().expect("directory");
        let program = directory.path().join("secret-tool-oversized-fixture");
        let block = "x".repeat(1_024);
        let blocks = MAX_SECRET_INPUT_BYTES / 1_024;
        fs::write(
            &program,
            format!(
                "#!/bin/sh\ni=0\nwhile [ \"$i\" -lt {blocks} ]; do\n  printf '%s' '{block}'\n  i=$((i + 1))\ndone\nprintf x\n"
            ),
        )
        .expect("script");
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).expect("permissions");
        let store = SecretToolStore { program };
        let reference = SecretRef::secret_service(Uuid::from_u128(901));

        let error = store
            .resolve_reference(&reference)
            .await
            .expect_err("oversized value must fail");
        assert!(
            matches!(
                &error,
                SecretStoreError::Backend(reason)
                    if reason.as_str() == "secret_service_value_too_large"
            ),
            "unexpected oversized lookup error: {error:?}"
        );
        let rendered = format!("{error:?} {error}");
        assert!(!rendered.contains(&"x".repeat(64)));
    }
}
