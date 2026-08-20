use crate::{
    client::{
        HEALTH_TOTAL_DEADLINE, REMOTE_SESSION_TOTAL_DEADLINE, SNAPSHOT_TOTAL_DEADLINE,
        SPEEDTEST_TOTAL_DEADLINE, SYSTEM_INFO_TOTAL_DEADLINE,
    },
    frame::{FrameError, MAX_FRAME_PAYLOAD_BYTES, WireBudget, read_json, write_frame},
    message::{
        ApplicationChunk, DaemonError, HealthReport, HealthRequest, MAX_APPLICATION_RECORDS,
        MAX_CHUNK_RECORDS, MAX_NOTES_EXPORT_FRAMES, MAX_NOTES_EXPORT_WIRE_BYTES,
        MAX_RESPONSE_FRAMES, MAX_RESPONSE_WIRE_BYTES, MAX_TOTAL_RECORDS, NetworkApplicationChunk,
        NetworkInterfaceChunk, NetworkSnapshotEnd, NetworkSnapshotStart, NoteSummaryChunk,
        NotesContentChunk, NotesContentEnd, NotesContentKind, NotesContentStart, NotesPageEnd,
        NotesPageStart, RequestBody, RequestEnvelope, ResponseBody, ResponseEnvelope, SnapshotEnd,
        SnapshotStart, SpeedTestStreamEvent, SystemInfoReport, TerminalStreamData,
        TerminalStreamEnd, TerminalStreamStart,
        TerminalStreamStatus, TransferLocalHandleBind, TransferPageEnd, TransferPageStart,
        TransferTaskChunk, UsageApplicationChunk, UsageSummaryEnd, UsageSummaryStart,
        WIRE_PROTOCOL_VERSION, speedtest_deep_deadline,
    },
    peer::{PeerError, verify_peer_uid},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use localdesk_domain::{
    Capability, CapabilityAvailability, CapabilityRuntime, HealthState, MAX_NETWORK_APPLICATIONS,
    MAX_NETWORK_INTERFACES, MAX_USAGE_APPLICATIONS, NETWORK_SCHEMA_VERSION,
    NOTE_CONTENT_CHUNK_BYTES, NOTES_SCHEMA_VERSION, NetworkSnapshot, NoteDocument, NoteExport,
    NotePage, NotesCommand, NotesOutput, RequestHealth, SpeedTestCancelResult,
    SpeedTestDeepCommand, SpeedTestDeepOutput, SpeedTestRunKind, TELEMETRY_SCHEMA_VERSION, TelemetrySnapshot,
    USAGE_SCHEMA_VERSION, UsageSummary,
    UsageSummaryQuery, aggregate_request_health, capability_catalog,
};
use localdesk_remote_core::{
    RemoteAdapterCatalog, RemoteProfileCommand, RemoteProfileResult, RemoteSessionCommand,
    RemoteSessionResult, SecretCommand, SecretCommandResult, TerminalCommand, TerminalRead,
    TerminalResult, TerminalState,
};
use localdesk_transfers::{
    TRANSFER_PUBLIC_SCHEMA_VERSION, TransferCommand, TransferLocalHandleGrant, TransferOutput,
    TransferPage,
};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, future::Future, io, pin::Pin, sync::Arc, time::Duration};
use thiserror::Error;
use tokio::{
    net::{UnixListener, UnixStream},
    sync::{Semaphore, mpsc, watch},
    task::JoinSet,
    time::{Instant, sleep, timeout, timeout_at},
};
use uuid::Uuid;

pub const MAX_CONNECTIONS: usize = 32;
pub const MAX_SNAPSHOT_STREAMS: usize = 4;
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(6);
const TERMINAL_STREAM_IDLE_INTERVAL: Duration = Duration::from_millis(16);
const TERMINAL_STREAM_STATUS_INTERVAL: Duration = Duration::from_millis(100);
const TERMINAL_STREAM_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);

pub type CapabilityProvider = Arc<dyn Fn() -> CapabilityRuntime + Send + Sync>;
pub type SnapshotProviderFuture = Pin<
    Box<dyn Future<Output = Result<TelemetrySnapshot, SnapshotProviderError>> + Send + 'static>,
>;
pub type SnapshotProvider = Arc<dyn Fn() -> SnapshotProviderFuture + Send + Sync>;
pub type NetworkSnapshotProviderFuture =
    Pin<Box<dyn Future<Output = Result<NetworkSnapshot, SnapshotProviderError>> + Send + 'static>>;
pub type NetworkSnapshotProvider = Arc<dyn Fn() -> NetworkSnapshotProviderFuture + Send + Sync>;
pub type UsageSummaryProviderFuture =
    Pin<Box<dyn Future<Output = Result<UsageSummary, SnapshotProviderError>> + Send + 'static>>;
pub type UsageSummaryProvider =
    Arc<dyn Fn(UsageSummaryQuery) -> UsageSummaryProviderFuture + Send + Sync>;
pub type RemoteCapabilitiesProviderFuture = Pin<
    Box<dyn Future<Output = Result<RemoteAdapterCatalog, SnapshotProviderError>> + Send + 'static>,
>;
pub type RemoteCapabilitiesProvider =
    Arc<dyn Fn() -> RemoteCapabilitiesProviderFuture + Send + Sync>;
pub type RemoteProfileProviderFuture = Pin<
    Box<dyn Future<Output = Result<RemoteProfileResult, SnapshotProviderError>> + Send + 'static>,
>;
pub type RemoteProfileProvider =
    Arc<dyn Fn(RemoteProfileCommand) -> RemoteProfileProviderFuture + Send + Sync>;
pub type SecretCommandProviderFuture = Pin<
    Box<dyn Future<Output = Result<SecretCommandResult, SnapshotProviderError>> + Send + 'static>,
>;
pub type SecretCommandProvider =
    Arc<dyn Fn(SecretCommand) -> SecretCommandProviderFuture + Send + Sync>;
pub type RemoteSessionProviderFuture = Pin<
    Box<dyn Future<Output = Result<RemoteSessionResult, SnapshotProviderError>> + Send + 'static>,
>;
pub type RemoteSessionProvider =
    Arc<dyn Fn(RemoteSessionCommand) -> RemoteSessionProviderFuture + Send + Sync>;
pub type NotesProviderFuture =
    Pin<Box<dyn Future<Output = Result<NotesOutput, SnapshotProviderError>> + Send + 'static>>;
pub type NotesProvider = Arc<dyn Fn(NotesCommand) -> NotesProviderFuture + Send + Sync>;
pub type TerminalProviderFuture =
    Pin<Box<dyn Future<Output = Result<TerminalResult, SnapshotProviderError>> + Send + 'static>>;
pub type TerminalProvider = Arc<dyn Fn(TerminalCommand) -> TerminalProviderFuture + Send + Sync>;
pub type TransferProviderFuture =
    Pin<Box<dyn Future<Output = Result<TransferOutput, SnapshotProviderError>> + Send + 'static>>;
pub type TransferProvider = Arc<dyn Fn(TransferCommand) -> TransferProviderFuture + Send + Sync>;
pub type TransferLocalHandleProviderFuture = Pin<
    Box<
        dyn Future<Output = Result<TransferLocalHandleGrant, SnapshotProviderError>>
            + Send
            + 'static,
    >,
>;
pub type TransferLocalHandleProvider =
    Arc<dyn Fn(TransferLocalHandleBind) -> TransferLocalHandleProviderFuture + Send + Sync>;
pub type SpeedTestProviderFuture =
    Pin<Box<dyn Future<Output = Result<mpsc::Receiver<SpeedTestStreamEvent>, SnapshotProviderError>> + Send + 'static>>;
pub type SpeedTestProvider =
    Arc<dyn Fn(Vec<localdesk_domain::SpeedTestStage>) -> SpeedTestProviderFuture + Send + Sync>;
pub type SpeedTestCancelProviderFuture =
    Pin<Box<dyn Future<Output = Result<SpeedTestCancelResult, SnapshotProviderError>> + Send + 'static>>;
pub type SpeedTestCancelProvider =
    Arc<dyn Fn(SpeedTestRunKind) -> SpeedTestCancelProviderFuture + Send + Sync>;
pub type SpeedTestDeepProviderFuture =
    Pin<Box<dyn Future<Output = Result<SpeedTestDeepOutput, SnapshotProviderError>> + Send + 'static>>;
pub type SpeedTestDeepProvider =
    Arc<dyn Fn(SpeedTestDeepCommand) -> SpeedTestDeepProviderFuture + Send + Sync>;
pub type SystemInfoProviderFuture = Pin<
    Box<dyn Future<Output = Result<SystemInfoReport, SnapshotProviderError>> + Send + 'static>,
>;
pub type SystemInfoProvider = Arc<dyn Fn() -> SystemInfoProviderFuture + Send + Sync>;

#[derive(Clone)]
pub struct ServerConfig {
    daemon_version: String,
    capability_provider: CapabilityProvider,
    snapshot_provider: Option<SnapshotProvider>,
    network_snapshot_provider: Option<NetworkSnapshotProvider>,
    usage_summary_provider: Option<UsageSummaryProvider>,
    remote_capabilities_provider: Option<RemoteCapabilitiesProvider>,
    system_info_provider: Option<SystemInfoProvider>,
    remote_profile_provider: Option<RemoteProfileProvider>,
    secret_command_provider: Option<SecretCommandProvider>,
    remote_session_provider: Option<RemoteSessionProvider>,
    notes_provider: Option<NotesProvider>,
    terminal_provider: Option<TerminalProvider>,
    transfer_provider: Option<TransferProvider>,
    transfer_local_handle_provider: Option<TransferLocalHandleProvider>,
    speedtest_provider: Option<SpeedTestProvider>,
    speedtest_cancel_provider: Option<SpeedTestCancelProvider>,
    speedtest_deep_provider: Option<SpeedTestDeepProvider>,
    notes_export_permit: Arc<Semaphore>,
    speedtest_permits: Arc<Semaphore>,
}

impl ServerConfig {
    pub fn new(daemon_version: impl Into<String>, capability_provider: CapabilityProvider) -> Self {
        Self {
            daemon_version: daemon_version.into(),
            capability_provider,
            snapshot_provider: None,
            network_snapshot_provider: None,
            usage_summary_provider: None,
            remote_capabilities_provider: None,
            system_info_provider: None,
            remote_profile_provider: None,
            secret_command_provider: None,
            remote_session_provider: None,
            notes_provider: None,
            terminal_provider: None,
            transfer_provider: None,
            transfer_local_handle_provider: None,
            speedtest_provider: None,
            speedtest_cancel_provider: None,
            speedtest_deep_provider: None,
            notes_export_permit: Arc::new(Semaphore::new(1)),
            speedtest_permits: Arc::new(Semaphore::new(2)),
        }
    }

    pub fn with_snapshot_provider(mut self, snapshot_provider: SnapshotProvider) -> Self {
        self.snapshot_provider = Some(snapshot_provider);
        self
    }

    pub fn with_network_snapshot_provider(mut self, provider: NetworkSnapshotProvider) -> Self {
        self.network_snapshot_provider = Some(provider);
        self
    }

    pub fn with_usage_summary_provider(mut self, provider: UsageSummaryProvider) -> Self {
        self.usage_summary_provider = Some(provider);
        self
    }

    pub fn with_remote_capabilities_provider(
        mut self,
        provider: RemoteCapabilitiesProvider,
    ) -> Self {
        self.remote_capabilities_provider = Some(provider);
        self
    }

    pub fn with_system_info_provider(mut self, provider: SystemInfoProvider) -> Self {
        self.system_info_provider = Some(provider);
        self
    }

    pub fn with_remote_profile_provider(mut self, provider: RemoteProfileProvider) -> Self {
        self.remote_profile_provider = Some(provider);
        self
    }

    pub fn with_secret_command_provider(mut self, provider: SecretCommandProvider) -> Self {
        self.secret_command_provider = Some(provider);
        self
    }

    pub fn with_remote_session_provider(mut self, provider: RemoteSessionProvider) -> Self {
        self.remote_session_provider = Some(provider);
        self
    }

    pub fn with_notes_provider(mut self, provider: NotesProvider) -> Self {
        self.notes_provider = Some(provider);
        self
    }

    pub fn with_terminal_provider(mut self, provider: TerminalProvider) -> Self {
        self.terminal_provider = Some(provider);
        self
    }

    pub fn with_transfer_provider(mut self, provider: TransferProvider) -> Self {
        self.transfer_provider = Some(provider);
        self
    }

    pub fn with_transfer_local_handle_provider(
        mut self,
        provider: TransferLocalHandleProvider,
    ) -> Self {
        self.transfer_local_handle_provider = Some(provider);
        self
    }

    pub fn with_speedtest_provider(mut self, provider: SpeedTestProvider) -> Self {
        self.speedtest_provider = Some(provider);
        self
    }

    pub fn with_speedtest_cancel_provider(mut self, provider: SpeedTestCancelProvider) -> Self {
        self.speedtest_cancel_provider = Some(provider);
        self
    }

    pub fn with_speedtest_deep_provider(mut self, provider: SpeedTestDeepProvider) -> Self {
        self.speedtest_deep_provider = Some(provider);
        self
    }
}

#[derive(Debug, Clone, Eq, Error, PartialEq)]
#[error("snapshot provider returned {code}: {reason}")]
pub struct SnapshotProviderError {
    pub code: String,
    pub reason: String,
    pub retryable: bool,
}

impl SnapshotProviderError {
    pub fn new(code: impl Into<String>, reason: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: code.into(),
            reason: reason.into(),
            retryable,
        }
    }

    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self::new("snapshot_provider_unavailable", reason, true)
    }
}

#[derive(Debug, Error)]
pub enum ServerError {
    #[error("server accept failed: {0}")]
    Accept(#[source] io::Error),
    #[error("server connection failed: {0}")]
    Connection(#[from] FrameError),
    #[error("server peer verification failed: {0}")]
    Peer(#[from] PeerError),
    #[error("server received an invalid request without a trustworthy identity")]
    Protocol,
}

pub async fn serve(
    listener: UnixListener,
    config: ServerConfig,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), ServerError> {
    let connection_permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let snapshot_permits = Arc::new(Semaphore::new(MAX_SNAPSHOT_STREAMS));
    let mut tasks = JoinSet::new();

    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            joined = tasks.join_next(), if !tasks.is_empty() => {
                if let Some(Err(error)) = joined {
                    tracing::debug!(%error, "appd connection task failed");
                }
            }
            accepted = listener.accept() => {
                let (stream, _) = accepted.map_err(ServerError::Accept)?;
                let accepted_at = Instant::now();
                let Ok(connection_permit) = connection_permits.clone().try_acquire_owned() else {
                    drop(stream);
                    continue;
                };
                let config = config.clone();
                let snapshot_permits = snapshot_permits.clone();
                tasks.spawn(async move {
                    let _connection_permit = connection_permit;
                    if let Err(error) = handle_connection_inner(
                        stream,
                        config,
                        snapshot_permits,
                        accepted_at,
                    ).await {
                        tracing::debug!(%error, "appd connection closed with error");
                    }
                });
            }
        }
    }

    if timeout(SHUTDOWN_GRACE, drain_tasks(&mut tasks))
        .await
        .is_err()
    {
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    }
    Ok(())
}

pub async fn handle_connection(
    stream: UnixStream,
    config: ServerConfig,
) -> Result<(), ServerError> {
    handle_connection_inner(
        stream,
        config,
        Arc::new(Semaphore::new(MAX_SNAPSHOT_STREAMS)),
        Instant::now(),
    )
    .await
}

async fn drain_tasks(tasks: &mut JoinSet<()>) {
    while tasks.join_next().await.is_some() {}
}

async fn handle_connection_inner(
    mut stream: UnixStream,
    config: ServerConfig,
    snapshot_permits: Arc<Semaphore>,
    accepted_at: Instant,
) -> Result<(), ServerError> {
    verify_peer_uid(&stream)?;
    let request_deadline = accepted_at + HEALTH_TOTAL_DEADLINE;
    let mut request_budget = WireBudget::new(1, MAX_FRAME_PAYLOAD_BYTES + 4);
    let request: RequestEnvelope =
        read_json(&mut stream, request_deadline, &mut request_budget).await?;
    if request.request_id.is_nil() {
        return Err(ServerError::Protocol);
    }
    if request.protocol_version != WIRE_PROTOCOL_VERSION {
        return write_terminal_error(
            &mut stream,
            request.request_id,
            request_deadline,
            DaemonError::new(
                "unsupported_protocol",
                "wire_protocol_version_must_be_13",
                false,
            ),
        )
        .await;
    }

    match request.body {
        RequestBody::Health(health_request) => {
            let deadline = accepted_at + HEALTH_TOTAL_DEADLINE;
            match build_health_report(&config, health_request) {
                Ok(report) => {
                    let response = ResponseEnvelope {
                        protocol_version: WIRE_PROTOCOL_VERSION,
                        request_id: request.request_id,
                        sequence: 0,
                        snapshot_id: None,
                        body: ResponseBody::HealthReport(report),
                    };
                    write_response(&mut stream, &response, deadline).await
                }
                Err(error) => {
                    write_terminal_error(&mut stream, request.request_id, deadline, error).await
                }
            }
        }
        RequestBody::TelemetrySnapshot(_) => {
            let deadline = accepted_at + SNAPSHOT_TOTAL_DEADLINE;
            let Ok(_snapshot_permit) = snapshot_permits.try_acquire_owned() else {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new(
                        "snapshot_capacity_exceeded",
                        "maximum_active_snapshot_streams_reached",
                        true,
                    ),
                )
                .await;
            };
            let Some(provider) = config.snapshot_provider else {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new(
                        "snapshot_provider_unavailable",
                        "snapshot_provider_unavailable",
                        true,
                    ),
                )
                .await;
            };
            let snapshot = match timeout_at(deadline, provider()).await {
                Ok(Ok(snapshot)) => snapshot,
                Ok(Err(error)) => {
                    return write_terminal_error(
                        &mut stream,
                        request.request_id,
                        deadline,
                        DaemonError::new(error.code, error.reason, error.retryable),
                    )
                    .await;
                }
                Err(_) => return Err(ServerError::Connection(FrameError::DeadlineExceeded)),
            };
            let plan = match SnapshotPlan::build(request.request_id, snapshot) {
                Ok(plan) => plan,
                Err(error) => {
                    return write_terminal_error(&mut stream, request.request_id, deadline, error)
                        .await;
                }
            };
            plan.write_to(&mut stream, deadline).await
        }
        RequestBody::NetworkSnapshot(_) => {
            let deadline = accepted_at + SNAPSHOT_TOTAL_DEADLINE;
            let Ok(_snapshot_permit) = snapshot_permits.try_acquire_owned() else {
                return write_capacity_error(&mut stream, request.request_id, deadline).await;
            };
            let Some(provider) = config.network_snapshot_provider else {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new(
                        "network_provider_unavailable",
                        "network_provider_unavailable",
                        true,
                    ),
                )
                .await;
            };
            let snapshot = match timeout_at(deadline, provider()).await {
                Ok(Ok(snapshot)) => snapshot,
                Ok(Err(error)) => {
                    return write_terminal_error(
                        &mut stream,
                        request.request_id,
                        deadline,
                        DaemonError::new(error.code, error.reason, error.retryable),
                    )
                    .await;
                }
                Err(_) => return Err(ServerError::Connection(FrameError::DeadlineExceeded)),
            };
            let plan = match NetworkSnapshotPlan::build(request.request_id, snapshot) {
                Ok(plan) => plan,
                Err(error) => {
                    return write_terminal_error(&mut stream, request.request_id, deadline, error)
                        .await;
                }
            };
            plan.write_to(&mut stream, deadline).await
        }
        RequestBody::UsageSummary(usage_request) => {
            let deadline = accepted_at + SNAPSHOT_TOTAL_DEADLINE;
            if let Err(reason) = usage_request.query.validate() {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new("invalid_request", reason, false),
                )
                .await;
            }
            let Ok(_snapshot_permit) = snapshot_permits.try_acquire_owned() else {
                return write_capacity_error(&mut stream, request.request_id, deadline).await;
            };
            let Some(provider) = config.usage_summary_provider else {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new(
                        "usage_provider_unavailable",
                        "usage_provider_unavailable",
                        true,
                    ),
                )
                .await;
            };
            let query = usage_request.query;
            let summary = match timeout_at(deadline, provider(query.clone())).await {
                Ok(Ok(summary)) => summary,
                Ok(Err(error)) => {
                    return write_terminal_error(
                        &mut stream,
                        request.request_id,
                        deadline,
                        DaemonError::new(error.code, error.reason, error.retryable),
                    )
                    .await;
                }
                Err(_) => return Err(ServerError::Connection(FrameError::DeadlineExceeded)),
            };
            if summary.query != query {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new(
                        "usage_query_mismatch",
                        "usage_provider_query_mismatch",
                        false,
                    ),
                )
                .await;
            }
            let plan = match UsageSummaryPlan::build(request.request_id, summary) {
                Ok(plan) => plan,
                Err(error) => {
                    return write_terminal_error(&mut stream, request.request_id, deadline, error)
                        .await;
                }
            };
            plan.write_to(&mut stream, deadline).await
        }
        RequestBody::RemoteCapabilities(_) => {
            let deadline = accepted_at + HEALTH_TOTAL_DEADLINE;
            let Some(provider) = config.remote_capabilities_provider else {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new(
                        "remote_capabilities_provider_unavailable",
                        "remote_capabilities_provider_unavailable",
                        true,
                    ),
                )
                .await;
            };
            let catalog = match timeout_at(deadline, provider()).await {
                Ok(Ok(catalog)) => catalog,
                Ok(Err(error)) => {
                    return write_terminal_error(
                        &mut stream,
                        request.request_id,
                        deadline,
                        DaemonError::new(error.code, error.reason, error.retryable),
                    )
                    .await;
                }
                Err(_) => return Err(ServerError::Connection(FrameError::DeadlineExceeded)),
            };
            if let Err(reason) = catalog.validate() {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new("remote_catalog_invalid", reason, false),
                )
                .await;
            }
            let response = ResponseEnvelope {
                protocol_version: WIRE_PROTOCOL_VERSION,
                request_id: request.request_id,
                sequence: 0,
                snapshot_id: None,
                body: ResponseBody::RemoteCapabilities(catalog),
            };
            write_response(&mut stream, &response, deadline).await
        }
        RequestBody::SystemInfo(_) => {
            let deadline = accepted_at + SYSTEM_INFO_TOTAL_DEADLINE;
            let Some(provider) = config.system_info_provider else {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new(
                        "system_info_provider_unavailable",
                        "system_info_provider_unavailable",
                        true,
                    ),
                )
                .await;
            };
            let report = match timeout_at(deadline, provider()).await {
                Ok(Ok(report)) => report,
                Ok(Err(error)) => {
                    return write_terminal_error(
                        &mut stream,
                        request.request_id,
                        deadline,
                        DaemonError::new(error.code, error.reason, error.retryable),
                    )
                    .await;
                }
                Err(_) => return Err(ServerError::Connection(FrameError::DeadlineExceeded)),
            };
            if !report.validate() {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new("system_info_invalid", "system_info_schema_version_mismatch", false),
                )
                .await;
            }
            let response = ResponseEnvelope {
                protocol_version: WIRE_PROTOCOL_VERSION,
                request_id: request.request_id,
                sequence: 0,
                snapshot_id: None,
                body: ResponseBody::SystemInfo(report),
            };
            write_response(&mut stream, &response, deadline).await
        }
        RequestBody::RemoteProfile(command) => {
            let deadline = accepted_at + HEALTH_TOTAL_DEADLINE;
            if command.validate().is_err() {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new("invalid_request", "remote_profile_command_invalid", false),
                )
                .await;
            }
            let Some(provider) = config.remote_profile_provider else {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new(
                        "remote_profile_provider_unavailable",
                        "remote_profile_provider_unavailable",
                        true,
                    ),
                )
                .await;
            };
            let result = match timeout_at(deadline, provider(command.clone())).await {
                Ok(Ok(result)) => result,
                Ok(Err(error)) => {
                    return write_terminal_error(
                        &mut stream,
                        request.request_id,
                        deadline,
                        DaemonError::new(error.code, error.reason, error.retryable),
                    )
                    .await;
                }
                Err(_) => return Err(ServerError::Connection(FrameError::DeadlineExceeded)),
            };
            if result.validate_for(&command).is_err() {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new(
                        "remote_profile_result_invalid",
                        "remote_profile_result_invalid",
                        false,
                    ),
                )
                .await;
            }
            let response = ResponseEnvelope {
                protocol_version: WIRE_PROTOCOL_VERSION,
                request_id: request.request_id,
                sequence: 0,
                snapshot_id: None,
                body: ResponseBody::RemoteProfile(result),
            };
            write_response(&mut stream, &response, deadline).await
        }
        RequestBody::Secret(command) => {
            let deadline = accepted_at + HEALTH_TOTAL_DEADLINE;
            if command.validate().is_err() {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new("invalid_request", "secret_command_invalid", false),
                )
                .await;
            }
            let Some(provider) = config.secret_command_provider else {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new(
                        "secret_store_unavailable",
                        "secret_store_provider_unavailable",
                        true,
                    ),
                )
                .await;
            };
            let result = match timeout_at(deadline, provider(command.clone())).await {
                Ok(Ok(result)) => result,
                Ok(Err(error)) => {
                    return write_terminal_error(
                        &mut stream,
                        request.request_id,
                        deadline,
                        DaemonError::new(error.code, error.reason, error.retryable),
                    )
                    .await;
                }
                Err(_) => return Err(ServerError::Connection(FrameError::DeadlineExceeded)),
            };
            if result.validate_for(&command).is_err() {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new(
                        "secret_store_result_invalid",
                        "secret_store_result_invalid",
                        false,
                    ),
                )
                .await;
            }
            let response = ResponseEnvelope {
                protocol_version: WIRE_PROTOCOL_VERSION,
                request_id: request.request_id,
                sequence: 0,
                snapshot_id: None,
                body: ResponseBody::Secret(result),
            };
            write_response(&mut stream, &response, deadline).await
        }
        RequestBody::RemoteSession(command) => {
            let deadline = accepted_at + REMOTE_SESSION_TOTAL_DEADLINE;
            if command.validate().is_err() {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new("invalid_request", "remote_session_command_invalid", false),
                )
                .await;
            }
            let Some(provider) = config.remote_session_provider else {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new(
                        "remote_session_provider_unavailable",
                        "remote_session_provider_unavailable",
                        true,
                    ),
                )
                .await;
            };
            let result = match timeout_at(deadline, provider(command.clone())).await {
                Ok(Ok(result)) => result,
                Ok(Err(error)) => {
                    return write_terminal_error(
                        &mut stream,
                        request.request_id,
                        deadline,
                        DaemonError::new(error.code, error.reason, error.retryable),
                    )
                    .await;
                }
                Err(_) => return Err(ServerError::Connection(FrameError::DeadlineExceeded)),
            };
            if result.validate_for(&command).is_err() {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new(
                        "remote_session_result_invalid",
                        "remote_session_result_invalid",
                        false,
                    ),
                )
                .await;
            }
            let response = ResponseEnvelope {
                protocol_version: WIRE_PROTOCOL_VERSION,
                request_id: request.request_id,
                sequence: 0,
                snapshot_id: None,
                body: ResponseBody::RemoteSession(result),
            };
            write_response(&mut stream, &response, deadline).await
        }
        RequestBody::Terminal(command) => {
            let deadline = accepted_at + SNAPSHOT_TOTAL_DEADLINE;
            if command.validate().is_err() {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new("invalid_request", "terminal_command_invalid", false),
                )
                .await;
            }
            let Some(provider) = config.terminal_provider else {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new(
                        "terminal_provider_unavailable",
                        "terminal_provider_unavailable",
                        true,
                    ),
                )
                .await;
            };
            if let TerminalCommand::Stream {
                session_id,
                max_bytes,
            } = command
            {
                return serve_terminal_stream(
                    &mut stream,
                    provider,
                    request.request_id,
                    session_id,
                    max_bytes,
                )
                .await;
            }
            let result = match timeout_at(deadline, provider(command.clone())).await {
                Ok(Ok(result)) => result,
                Ok(Err(error)) => {
                    return write_terminal_error(
                        &mut stream,
                        request.request_id,
                        deadline,
                        DaemonError::new(error.code, error.reason, error.retryable),
                    )
                    .await;
                }
                Err(_) => return Err(ServerError::Connection(FrameError::DeadlineExceeded)),
            };
            if result.validate_for(&command).is_err() {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new("terminal_result_invalid", "terminal_result_invalid", false),
                )
                .await;
            }
            write_response(
                &mut stream,
                &ResponseEnvelope {
                    protocol_version: WIRE_PROTOCOL_VERSION,
                    request_id: request.request_id,
                    sequence: 0,
                    snapshot_id: None,
                    body: ResponseBody::Terminal(result),
                },
                deadline,
            )
            .await
        }
        RequestBody::Transfer(command) => {
            let deadline = accepted_at + SNAPSHOT_TOTAL_DEADLINE;
            if command.validate().is_err() {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new("invalid_request", "transfer_command_invalid", false),
                )
                .await;
            }
            let _stream_permit = if matches!(command, TransferCommand::List { .. }) {
                match snapshot_permits.try_acquire_owned() {
                    Ok(permit) => Some(permit),
                    Err(_) => {
                        return write_capacity_error(&mut stream, request.request_id, deadline)
                            .await;
                    }
                }
            } else {
                None
            };
            let Some(provider) = config.transfer_provider else {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new(
                        "transfer_provider_unavailable",
                        "transfer_provider_unavailable",
                        true,
                    ),
                )
                .await;
            };
            let output = match timeout_at(deadline, provider(command.clone())).await {
                Ok(Ok(output)) => output,
                Ok(Err(error)) => {
                    return write_terminal_error(
                        &mut stream,
                        request.request_id,
                        deadline,
                        DaemonError::new(error.code, error.reason, error.retryable),
                    )
                    .await;
                }
                Err(_) => return Err(ServerError::Connection(FrameError::DeadlineExceeded)),
            };
            if output.validate_for(&command).is_err() {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new(
                        "transfer_result_invalid",
                        "transfer_result_did_not_match_command",
                        false,
                    ),
                )
                .await;
            }
            match output {
                TransferOutput::Page { page } => {
                    match TransferPlan::page(request.request_id, page) {
                        Ok(plan) => plan.write_to(&mut stream, deadline).await,
                        Err(error) => {
                            write_terminal_error(&mut stream, request.request_id, deadline, error)
                                .await
                        }
                    }
                }
                output @ (TransferOutput::Task { .. } | TransferOutput::Mutation { .. }) => {
                    write_response(
                        &mut stream,
                        &ResponseEnvelope {
                            protocol_version: WIRE_PROTOCOL_VERSION,
                            request_id: request.request_id,
                            sequence: 0,
                            snapshot_id: None,
                            body: ResponseBody::Transfer(Box::new(output)),
                        },
                        deadline,
                    )
                    .await
                }
            }
        }
        RequestBody::TransferLocalHandle(bind) => {
            let deadline = accepted_at + SNAPSHOT_TOTAL_DEADLINE;
            if !bind.validate() {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new(
                        "invalid_request",
                        "transfer_local_handle_bind_invalid",
                        false,
                    ),
                )
                .await;
            }
            let Some(provider) = config.transfer_local_handle_provider else {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new(
                        "transfer_local_handle_provider_unavailable",
                        "transfer_local_handle_provider_unavailable",
                        true,
                    ),
                )
                .await;
            };
            let grant = match timeout_at(deadline, provider(bind)).await {
                Ok(Ok(grant)) => grant,
                Ok(Err(error)) => {
                    return write_terminal_error(
                        &mut stream,
                        request.request_id,
                        deadline,
                        DaemonError::new(error.code, error.reason, error.retryable),
                    )
                    .await;
                }
                Err(_) => return Err(ServerError::Connection(FrameError::DeadlineExceeded)),
            };
            if grant.validate().is_err() {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new(
                        "transfer_local_handle_result_invalid",
                        "transfer_local_handle_result_invalid",
                        false,
                    ),
                )
                .await;
            }
            write_response(
                &mut stream,
                &ResponseEnvelope {
                    protocol_version: WIRE_PROTOCOL_VERSION,
                    request_id: request.request_id,
                    sequence: 0,
                    snapshot_id: None,
                    body: ResponseBody::TransferLocalHandle(grant),
                },
                deadline,
            )
            .await
        }
        RequestBody::SpeedTestBasic(speedtest_request) => {
            let deadline = accepted_at + SPEEDTEST_TOTAL_DEADLINE;
            if speedtest_request.validate().is_err() {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new("invalid_request", "speedtest_stages_invalid", false),
                )
                .await;
            }
            let stages = speedtest_request.stages;
            let Ok(_speedtest_permit) = config.speedtest_permits.clone().try_acquire_owned() else {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new("speedtest_busy", "speedtest_already_running", true),
                )
                .await;
            };
            let Some(provider) = config.speedtest_provider else {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new(
                        "speedtest_provider_unavailable",
                        "speedtest_provider_unavailable",
                        true,
                    ),
                )
                .await;
            };
            let mut receiver = match timeout_at(deadline, provider(stages)).await {
                Ok(Ok(receiver)) => receiver,
                Ok(Err(error)) => {
                    return write_terminal_error(
                        &mut stream,
                        request.request_id,
                        deadline,
                        DaemonError::new(error.code, error.reason, error.retryable),
                    )
                    .await;
                }
                Err(_) => return Err(ServerError::Connection(FrameError::DeadlineExceeded)),
            };
            serve_speedtest_stream(&mut stream, &mut receiver, request.request_id, deadline).await
        }
        RequestBody::SpeedTestCancel(cancel_request) => {
            let deadline = accepted_at + SNAPSHOT_TOTAL_DEADLINE;
            let Some(provider) = config.speedtest_cancel_provider else {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new(
                        "speedtest_cancel_provider_unavailable",
                        "speedtest_cancel_provider_unavailable",
                        true,
                    ),
                )
                .await;
            };
            let result = match timeout_at(deadline, provider(cancel_request.run_kind)).await {
                Ok(Ok(result)) => result,
                Ok(Err(error)) => {
                    return write_terminal_error(
                        &mut stream,
                        request.request_id,
                        deadline,
                        DaemonError::new(error.code, error.reason, error.retryable),
                    )
                    .await;
                }
                Err(_) => return Err(ServerError::Connection(FrameError::DeadlineExceeded)),
            };
            if !result.validate() {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new(
                        "speedtest_cancel_result_invalid",
                        "speedtest_cancel_result_invalid",
                        false,
                    ),
                )
                .await;
            }
            write_response(
                &mut stream,
                &ResponseEnvelope {
                    protocol_version: WIRE_PROTOCOL_VERSION,
                    request_id: request.request_id,
                    sequence: 0,
                    snapshot_id: None,
                    body: ResponseBody::SpeedTestCancelled(result),
                },
                deadline,
            )
            .await
        }
        RequestBody::SpeedTestDeep(command) => {
            let deadline = accepted_at + speedtest_deep_deadline(&command);
            if command.validate().is_err() {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new(
                        "invalid_request",
                        "speedtest_deep_command_invalid",
                        false,
                    ),
                )
                .await;
            }
            let Some(provider) = config.speedtest_deep_provider else {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new(
                        "speedtest_deep_provider_unavailable",
                        "speedtest_deep_provider_unavailable",
                        true,
                    ),
                )
                .await;
            };
            let output = match timeout_at(deadline, provider(command.clone())).await {
                Ok(Ok(output)) => output,
                Ok(Err(error)) => {
                    return write_terminal_error(
                        &mut stream,
                        request.request_id,
                        deadline,
                        DaemonError::new(error.code, error.reason, error.retryable),
                    )
                    .await;
                }
                Err(_) => return Err(ServerError::Connection(FrameError::DeadlineExceeded)),
            };
            if !output.validate_for(&command) {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new(
                        "speedtest_deep_result_invalid",
                        "speedtest_deep_result_did_not_match_command",
                        false,
                    ),
                )
                .await;
            }
            write_response(
                &mut stream,
                &ResponseEnvelope {
                    protocol_version: WIRE_PROTOCOL_VERSION,
                    request_id: request.request_id,
                    sequence: 0,
                    snapshot_id: None,
                    body: ResponseBody::SpeedTestDeep(Box::new(output)),
                },
                deadline,
            )
            .await
        }
        RequestBody::Notes(command) => {
            let deadline = accepted_at + SNAPSHOT_TOTAL_DEADLINE;
            if let Err(reason) = command.validate() {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new("invalid_request", reason, false),
                )
                .await;
            }
            let Ok(_stream_permit) = snapshot_permits.try_acquire_owned() else {
                return write_capacity_error(&mut stream, request.request_id, deadline).await;
            };
            let _export_permit = if matches!(command, NotesCommand::Export { .. }) {
                match config.notes_export_permit.clone().try_acquire_owned() {
                    Ok(permit) => Some(permit),
                    Err(_) => {
                        return write_terminal_error(
                            &mut stream,
                            request.request_id,
                            deadline,
                            DaemonError::new(
                                "notes_export_capacity_exceeded",
                                "maximum_active_notes_export_streams_reached",
                                true,
                            ),
                        )
                        .await;
                    }
                }
            } else {
                None
            };
            let Some(provider) = config.notes_provider else {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new(
                        "notes_provider_unavailable",
                        "notes_provider_unavailable",
                        true,
                    ),
                )
                .await;
            };
            let output = match timeout_at(deadline, provider(command.clone())).await {
                Ok(Ok(output)) => output,
                Ok(Err(error)) => {
                    return write_terminal_error(
                        &mut stream,
                        request.request_id,
                        deadline,
                        DaemonError::new(error.code, error.reason, error.retryable),
                    )
                    .await;
                }
                Err(_) => return Err(ServerError::Connection(FrameError::DeadlineExceeded)),
            };
            if !notes_output_matches(&command, &output) {
                return write_terminal_error(
                    &mut stream,
                    request.request_id,
                    deadline,
                    DaemonError::new(
                        "notes_result_invalid",
                        "notes_result_did_not_match_command",
                        false,
                    ),
                )
                .await;
            }
            match output {
                NotesOutput::Mutation(result) => {
                    let response = ResponseEnvelope {
                        protocol_version: WIRE_PROTOCOL_VERSION,
                        request_id: request.request_id,
                        sequence: 0,
                        snapshot_id: None,
                        body: ResponseBody::NotesMutation(result),
                    };
                    write_response(&mut stream, &response, deadline).await
                }
                NotesOutput::Page(page) => match NotesPlan::page(request.request_id, page) {
                    Ok(plan) => plan.write_to(&mut stream, deadline).await,
                    Err(error) => {
                        write_terminal_error(&mut stream, request.request_id, deadline, error).await
                    }
                },
                NotesOutput::Document(document) => {
                    match NotesPlan::document(request.request_id, document) {
                        Ok(plan) => plan.write_to(&mut stream, deadline).await,
                        Err(error) => {
                            write_terminal_error(&mut stream, request.request_id, deadline, error)
                                .await
                        }
                    }
                }
                NotesOutput::Export(export) => {
                    match NotesPlan::export(request.request_id, export) {
                        Ok(plan) => plan.write_to(&mut stream, deadline).await,
                        Err(error) => {
                            write_terminal_error(&mut stream, request.request_id, deadline, error)
                                .await
                        }
                    }
                }
            }
        }
    }
}

fn notes_output_matches(command: &NotesCommand, output: &NotesOutput) -> bool {
    matches!(
        (command, output),
        (NotesCommand::List { .. }, NotesOutput::Page(_))
            | (NotesCommand::Get { .. }, NotesOutput::Document(_))
            | (NotesCommand::Export { .. }, NotesOutput::Export(_))
            | (
                NotesCommand::WriteInline { .. }
                    | NotesCommand::Delete { .. }
                    | NotesCommand::Restore { .. }
                    | NotesCommand::UploadBegin { .. }
                    | NotesCommand::UploadAppend { .. }
                    | NotesCommand::UploadCommit { .. }
                    | NotesCommand::UploadAbort { .. },
                NotesOutput::Mutation(_)
            )
    )
}

struct TransferPlan {
    encoded_frames: Vec<Vec<u8>>,
}

impl TransferPlan {
    fn page(request_id: Uuid, page: TransferPage) -> Result<Self, DaemonError> {
        page.validate().map_err(|_| {
            DaemonError::new("transfer_page_invalid", "transfer_page_invalid", false)
        })?;
        let snapshot_id = Uuid::new_v4();
        let total_tasks = checked_u32(page.tasks.len(), "transfer_task_count")?;
        let mut sequence = 1_u32;
        let mut chunks = Vec::new();
        encode_record_chunks(
            &page.tasks,
            request_id,
            snapshot_id,
            &mut sequence,
            &mut chunks,
            |records| ResponseBody::TransferTaskChunk(TransferTaskChunk { records }),
            "transfer_task",
        )?;
        let data_frame_count = checked_u32(chunks.len(), "transfer_data_frame_count")?;
        let start = TransferPageStart {
            schema_version: TRANSFER_PUBLIC_SCHEMA_VERSION,
            query: page.query.clone(),
            total_tasks,
            data_frame_count,
            has_more: page.has_more,
            next_offset: page.next_offset,
        };
        let end = TransferPageEnd {
            schema_version: TRANSFER_PUBLIC_SCHEMA_VERSION,
            query: page.query,
            total_tasks,
            data_frame_count,
            has_more: page.has_more,
            next_offset: page.next_offset,
        };
        let mut encoded_frames = Vec::with_capacity(chunks.len().saturating_add(2));
        encoded_frames.push(encode_response(&ResponseEnvelope {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id,
            sequence: 0,
            snapshot_id: Some(snapshot_id),
            body: ResponseBody::TransferPageStart(Box::new(start)),
        })?);
        encoded_frames.extend(chunks);
        encoded_frames.push(encode_response(&ResponseEnvelope {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id,
            sequence,
            snapshot_id: Some(snapshot_id),
            body: ResponseBody::TransferPageEnd(end),
        })?);
        validate_encoded_plan(&encoded_frames)?;
        Ok(Self { encoded_frames })
    }

    async fn write_to(self, stream: &mut UnixStream, deadline: Instant) -> Result<(), ServerError> {
        write_encoded_plan(self.encoded_frames, stream, deadline).await
    }
}

async fn write_capacity_error(
    stream: &mut UnixStream,
    request_id: Uuid,
    deadline: Instant,
) -> Result<(), ServerError> {
    write_terminal_error(
        stream,
        request_id,
        deadline,
        DaemonError::new(
            "snapshot_capacity_exceeded",
            "maximum_active_snapshot_streams_reached",
            true,
        ),
    )
    .await
}

fn build_health_report(
    config: &ServerConfig,
    request: HealthRequest,
) -> Result<HealthReport, DaemonError> {
    validate_requested_capabilities(&request.requested_capabilities)?;
    let runtime = (config.capability_provider)();
    let catalog = capability_catalog(&runtime);
    let capabilities = request
        .requested_capabilities
        .iter()
        .map(|requested| {
            catalog
                .iter()
                .find(|capability| capability.id == *requested)
                .cloned()
                .unwrap_or_else(|| {
                    Capability::new(
                        requested,
                        CapabilityAvailability::Unsupported,
                        "unknown_capability",
                    )
                })
        })
        .collect::<Vec<_>>();
    let daemon_health = match runtime.appd_health.status {
        CapabilityAvailability::Healthy => {
            RequestHealth::new(HealthState::Healthy, runtime.appd_health.reason)
        }
        CapabilityAvailability::Degraded => {
            RequestHealth::new(HealthState::Degraded, runtime.appd_health.reason)
        }
        CapabilityAvailability::Unsupported => {
            RequestHealth::new(HealthState::Unsupported, runtime.appd_health.reason)
        }
        CapabilityAvailability::Unreachable => RequestHealth::new(
            HealthState::Degraded,
            "daemon_runtime_reported_unreachable_while_responding",
        ),
    };
    let request_health = aggregate_request_health(Some(daemon_health), &capabilities)
        .expect("daemon health is present");
    Ok(HealthReport {
        daemon_version: config.daemon_version.clone(),
        health: request_health.health,
        reason: request_health.reason,
        capabilities,
    })
}

fn validate_requested_capabilities(requested: &[String]) -> Result<(), DaemonError> {
    if requested.is_empty() {
        return Err(DaemonError::new(
            "invalid_request",
            "requested_capabilities_must_not_be_empty",
            false,
        ));
    }
    if requested.len() > crate::message::MAX_REQUESTED_CAPABILITIES {
        return Err(DaemonError::new(
            "invalid_request",
            "requested_capabilities_exceeds_32",
            false,
        ));
    }
    if requested.iter().any(|capability| capability.is_empty()) {
        return Err(DaemonError::new(
            "invalid_request",
            "requested_capabilities_contains_empty",
            false,
        ));
    }
    let mut unique = HashSet::with_capacity(requested.len());
    if requested
        .iter()
        .any(|capability| !unique.insert(capability))
    {
        return Err(DaemonError::new(
            "invalid_request",
            "requested_capabilities_contains_duplicate",
            false,
        ));
    }
    Ok(())
}

struct NotesPlan {
    encoded_frames: Vec<Vec<u8>>,
}

impl NotesPlan {
    fn page(request_id: Uuid, page: NotePage) -> Result<Self, DaemonError> {
        page.validate()
            .map_err(|reason| DaemonError::new("notes_page_invalid", reason, false))?;
        let snapshot_id = Uuid::new_v4();
        let mut chunks = Vec::new();
        let mut offset = 0_usize;
        let mut sequence = 1_u32;
        while offset < page.notes.len() {
            let (selected, encoded) = select_note_summary_chunk(
                &page.notes[offset..],
                request_id,
                snapshot_id,
                sequence,
            )?;
            offset = offset.checked_add(selected).ok_or_else(|| {
                DaemonError::new("notes_counter_overflow", "notes_offset_overflow", false)
            })?;
            chunks.push(encoded);
            sequence = sequence.checked_add(1).ok_or_else(|| {
                DaemonError::new("notes_sequence_overflow", "notes_sequence_overflow", false)
            })?;
        }

        let total_notes = checked_u32(page.notes.len(), "notes_record_count")?;
        let data_frame_count = checked_u32(chunks.len(), "notes_data_frame_count")?;
        let start = NotesPageStart {
            schema_version: NOTES_SCHEMA_VERSION,
            query: page.query.clone(),
            total_notes,
            data_frame_count,
            has_more: page.has_more,
            next_offset: page.next_offset,
        };
        let end = NotesPageEnd {
            schema_version: NOTES_SCHEMA_VERSION,
            query: page.query,
            total_notes,
            data_frame_count,
            has_more: page.has_more,
            next_offset: page.next_offset,
        };
        let mut encoded_frames = Vec::with_capacity(chunks.len().saturating_add(2));
        encoded_frames.push(encode_response(&ResponseEnvelope {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id,
            sequence: 0,
            snapshot_id: Some(snapshot_id),
            body: ResponseBody::NotesPageStart(Box::new(start)),
        })?);
        encoded_frames.extend(chunks);
        encoded_frames.push(encode_response(&ResponseEnvelope {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id,
            sequence,
            snapshot_id: Some(snapshot_id),
            body: ResponseBody::NotesPageEnd(end),
        })?);
        validate_notes_encoded_plan(
            &encoded_frames,
            MAX_RESPONSE_FRAMES,
            MAX_RESPONSE_WIRE_BYTES,
        )?;
        Ok(Self { encoded_frames })
    }

    fn document(request_id: Uuid, document: NoteDocument) -> Result<Self, DaemonError> {
        document
            .validate()
            .map_err(|reason| DaemonError::new("notes_document_invalid", reason, false))?;
        let content_sha256 = document.summary.body_sha256.clone();
        Self::content(
            request_id,
            NotesContentKind::Document,
            Some(document.summary),
            document.body_markdown,
            content_sha256,
            MAX_RESPONSE_FRAMES,
            MAX_RESPONSE_WIRE_BYTES,
        )
    }

    fn export(request_id: Uuid, export: NoteExport) -> Result<Self, DaemonError> {
        export
            .validate()
            .map_err(|reason| DaemonError::new("notes_export_invalid", reason, false))?;
        Self::content(
            request_id,
            NotesContentKind::Export {
                format: export.format,
            },
            None,
            export.content,
            export.content_sha256,
            MAX_NOTES_EXPORT_FRAMES,
            MAX_NOTES_EXPORT_WIRE_BYTES,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn content(
        request_id: Uuid,
        kind: NotesContentKind,
        note: Option<localdesk_domain::NoteSummary>,
        content: String,
        content_sha256: String,
        max_frames: usize,
        max_wire_bytes: usize,
    ) -> Result<Self, DaemonError> {
        let actual_sha256 = format!("{:x}", Sha256::digest(content.as_bytes()));
        if actual_sha256 != content_sha256 {
            return Err(DaemonError::new(
                "notes_content_invalid",
                "notes_content_sha256_mismatch",
                false,
            ));
        }
        let total_bytes = checked_u32(content.len(), "notes_content_bytes")?;
        let snapshot_id = Uuid::new_v4();
        let mut chunks = Vec::new();
        let mut sequence = 1_u32;
        let mut offset = 0_usize;
        for bytes in content.as_bytes().chunks(NOTE_CONTENT_CHUNK_BYTES) {
            let encoded = encode_response(&ResponseEnvelope {
                protocol_version: WIRE_PROTOCOL_VERSION,
                request_id,
                sequence,
                snapshot_id: Some(snapshot_id),
                body: ResponseBody::NotesContentChunk(NotesContentChunk {
                    offset: checked_u32(offset, "notes_chunk_offset")?,
                    raw_bytes: checked_u32(bytes.len(), "notes_chunk_bytes")?,
                    data_base64: STANDARD.encode(bytes),
                }),
            })?;
            chunks.push(encoded);
            offset = offset.checked_add(bytes.len()).ok_or_else(|| {
                DaemonError::new(
                    "notes_counter_overflow",
                    "notes_content_offset_overflow",
                    false,
                )
            })?;
            sequence = sequence.checked_add(1).ok_or_else(|| {
                DaemonError::new("notes_sequence_overflow", "notes_sequence_overflow", false)
            })?;
        }
        let data_frame_count = checked_u32(chunks.len(), "notes_data_frame_count")?;
        let start = NotesContentStart {
            schema_version: NOTES_SCHEMA_VERSION,
            kind: kind.clone(),
            note,
            total_bytes,
            content_sha256: content_sha256.clone(),
            data_frame_count,
        };
        let end = NotesContentEnd {
            schema_version: NOTES_SCHEMA_VERSION,
            kind,
            total_bytes,
            content_sha256,
            data_frame_count,
        };
        let mut encoded_frames = Vec::with_capacity(chunks.len().saturating_add(2));
        encoded_frames.push(encode_response(&ResponseEnvelope {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id,
            sequence: 0,
            snapshot_id: Some(snapshot_id),
            body: ResponseBody::NotesContentStart(Box::new(start)),
        })?);
        encoded_frames.extend(chunks);
        encoded_frames.push(encode_response(&ResponseEnvelope {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id,
            sequence,
            snapshot_id: Some(snapshot_id),
            body: ResponseBody::NotesContentEnd(end),
        })?);
        validate_notes_encoded_plan(&encoded_frames, max_frames, max_wire_bytes)?;
        Ok(Self { encoded_frames })
    }

    async fn write_to(self, stream: &mut UnixStream, deadline: Instant) -> Result<(), ServerError> {
        write_encoded_plan(self.encoded_frames, stream, deadline).await
    }
}

fn select_note_summary_chunk(
    records: &[localdesk_domain::NoteSummary],
    request_id: Uuid,
    snapshot_id: Uuid,
    sequence: u32,
) -> Result<(usize, Vec<u8>), DaemonError> {
    let limit = records.len().min(MAX_CHUNK_RECORDS);
    let mut selected = None;
    for count in 1..=limit {
        match encode_response(&ResponseEnvelope {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id,
            sequence,
            snapshot_id: Some(snapshot_id),
            body: ResponseBody::NoteSummaryChunk(NoteSummaryChunk {
                records: records[..count].to_vec(),
            }),
        }) {
            Ok(encoded) => selected = Some((count, encoded)),
            Err(_) if count > 1 => break,
            Err(error) => return Err(error),
        }
    }
    selected.ok_or_else(|| {
        DaemonError::new(
            "notes_chunk_invalid",
            "notes_summary_chunk_must_not_be_empty",
            false,
        )
    })
}

fn validate_notes_encoded_plan(
    encoded_frames: &[Vec<u8>],
    max_frames: usize,
    max_wire_bytes: usize,
) -> Result<(), DaemonError> {
    if encoded_frames.len() > max_frames {
        return Err(DaemonError::new(
            "notes_frame_limit",
            "notes_response_frame_limit_exceeded",
            false,
        ));
    }
    let mut wire_bytes = 0_usize;
    for payload in encoded_frames {
        if payload.is_empty() || payload.len() > MAX_FRAME_PAYLOAD_BYTES {
            return Err(DaemonError::new(
                "notes_frame_limit",
                "notes_frame_payload_invalid",
                false,
            ));
        }
        wire_bytes = wire_bytes
            .checked_add(4)
            .and_then(|total| total.checked_add(payload.len()))
            .ok_or_else(|| {
                DaemonError::new("notes_wire_limit", "notes_wire_bytes_overflow", false)
            })?;
        if wire_bytes > max_wire_bytes {
            return Err(DaemonError::new(
                "notes_wire_limit",
                "notes_response_wire_limit_exceeded",
                false,
            ));
        }
    }
    Ok(())
}

struct NetworkSnapshotPlan {
    encoded_frames: Vec<Vec<u8>>,
}

impl NetworkSnapshotPlan {
    fn build(request_id: Uuid, snapshot: NetworkSnapshot) -> Result<Self, DaemonError> {
        if snapshot.schema_version != NETWORK_SCHEMA_VERSION {
            return Err(DaemonError::new(
                "network_schema_unsupported",
                "network_schema_must_be_1",
                false,
            ));
        }
        if snapshot.snapshot_id.is_nil() {
            return Err(DaemonError::new(
                "snapshot_identity_invalid",
                "snapshot_id_must_not_be_nil",
                false,
            ));
        }
        if snapshot.interfaces.len() > MAX_NETWORK_INTERFACES {
            return Err(DaemonError::new(
                "network_interface_limit",
                "network_interfaces_exceeds_256",
                false,
            ));
        }
        if snapshot.applications.len() > MAX_NETWORK_APPLICATIONS {
            return Err(DaemonError::new(
                "network_application_limit",
                "network_applications_exceeds_1024",
                false,
            ));
        }
        snapshot
            .validate()
            .map_err(|reason| DaemonError::new("network_snapshot_invalid", reason, false))?;

        let snapshot_id = snapshot.snapshot_id;
        let mut chunks = Vec::new();
        let mut sequence = 1_u32;
        encode_record_chunks(
            &snapshot.interfaces,
            request_id,
            snapshot_id,
            &mut sequence,
            &mut chunks,
            |records| ResponseBody::NetworkInterfaceChunk(NetworkInterfaceChunk { records }),
            "network_interface",
        )?;
        encode_record_chunks(
            &snapshot.applications,
            request_id,
            snapshot_id,
            &mut sequence,
            &mut chunks,
            |records| ResponseBody::NetworkApplicationChunk(NetworkApplicationChunk { records }),
            "network_application",
        )?;

        let total_interfaces = checked_u32(snapshot.interfaces.len(), "network_interface_count")?;
        let total_applications =
            checked_u32(snapshot.applications.len(), "network_application_count")?;
        let total_records_usize = snapshot
            .interfaces
            .len()
            .checked_add(snapshot.applications.len())
            .ok_or_else(|| {
                DaemonError::new(
                    "snapshot_record_limit",
                    "network_record_count_overflow",
                    false,
                )
            })?;
        let total_records = checked_u32(total_records_usize, "network_record_count")?;
        let data_frame_count = checked_u32(chunks.len(), "network_data_frame_count")?;
        let response_frames = chunks.len().checked_add(2).ok_or_else(|| {
            DaemonError::new(
                "snapshot_frame_limit",
                "snapshot_frame_count_overflow",
                false,
            )
        })?;
        if response_frames > MAX_RESPONSE_FRAMES {
            return Err(DaemonError::new(
                "snapshot_frame_limit",
                "snapshot_response_frames_exceeds_130",
                false,
            ));
        }

        let start = NetworkSnapshotStart {
            schema_version: snapshot.schema_version,
            captured_at_unix_ms: snapshot.captured_at_unix_ms,
            observed_boottime_ms: snapshot.observed_boottime_ms,
            sample_interval_ms: snapshot.sample_interval_ms,
            last_success_at_unix_ms: snapshot.last_success_at_unix_ms,
            freshness: snapshot.freshness,
            retryable: snapshot.retryable,
            system_traffic: snapshot.system_traffic,
            per_application: snapshot.per_application,
            coverage: snapshot.coverage,
            totals: snapshot.totals,
            aggregate_rate: snapshot.aggregate_rate,
            total_interfaces,
            total_applications,
            total_records,
            data_frame_count,
        };
        let end = NetworkSnapshotEnd {
            schema_version: snapshot.schema_version,
            total_interfaces,
            total_applications,
            total_records,
            data_frame_count,
        };
        let start = encode_response(&ResponseEnvelope {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id,
            sequence: 0,
            snapshot_id: Some(snapshot_id),
            body: ResponseBody::NetworkSnapshotStart(Box::new(start)),
        })?;
        let end = encode_response(&ResponseEnvelope {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id,
            sequence,
            snapshot_id: Some(snapshot_id),
            body: ResponseBody::NetworkSnapshotEnd(end),
        })?;
        let mut encoded_frames = Vec::with_capacity(response_frames);
        encoded_frames.push(start);
        encoded_frames.extend(chunks);
        encoded_frames.push(end);
        validate_encoded_plan(&encoded_frames)?;
        Ok(Self { encoded_frames })
    }

    async fn write_to(self, stream: &mut UnixStream, deadline: Instant) -> Result<(), ServerError> {
        write_encoded_plan(self.encoded_frames, stream, deadline).await
    }
}

struct UsageSummaryPlan {
    encoded_frames: Vec<Vec<u8>>,
}

impl UsageSummaryPlan {
    fn build(request_id: Uuid, summary: UsageSummary) -> Result<Self, DaemonError> {
        if summary.schema_version != USAGE_SCHEMA_VERSION {
            return Err(DaemonError::new(
                "usage_schema_unsupported",
                "usage_schema_must_be_2",
                false,
            ));
        }
        if summary.snapshot_id.is_nil() {
            return Err(DaemonError::new(
                "snapshot_identity_invalid",
                "snapshot_id_must_not_be_nil",
                false,
            ));
        }
        if summary.applications.len() > MAX_USAGE_APPLICATIONS {
            return Err(DaemonError::new(
                "usage_application_limit",
                "usage_applications_exceeds_1024",
                false,
            ));
        }
        summary
            .validate()
            .map_err(|reason| DaemonError::new("usage_summary_invalid", reason, false))?;

        let snapshot_id = summary.snapshot_id;
        let mut chunks = Vec::new();
        let mut sequence = 1_u32;
        encode_record_chunks(
            &summary.applications,
            request_id,
            snapshot_id,
            &mut sequence,
            &mut chunks,
            |records| ResponseBody::UsageApplicationChunk(UsageApplicationChunk { records }),
            "usage_application",
        )?;
        let total_applications =
            checked_u32(summary.applications.len(), "usage_application_count")?;
        let data_frame_count = checked_u32(chunks.len(), "usage_data_frame_count")?;
        let response_frames = chunks.len().checked_add(2).ok_or_else(|| {
            DaemonError::new(
                "snapshot_frame_limit",
                "snapshot_frame_count_overflow",
                false,
            )
        })?;
        if response_frames > MAX_RESPONSE_FRAMES {
            return Err(DaemonError::new(
                "snapshot_frame_limit",
                "snapshot_response_frames_exceeds_130",
                false,
            ));
        }
        let start = UsageSummaryStart {
            schema_version: summary.schema_version,
            captured_at_unix_ms: summary.captured_at_unix_ms,
            query: summary.query.clone(),
            status: summary.status,
            reason: summary.reason.clone(),
            retryable: summary.retryable,
            coverage: summary.coverage,
            total_applications,
            total_records: total_applications,
            data_frame_count,
        };
        let end = UsageSummaryEnd {
            schema_version: summary.schema_version,
            query: summary.query,
            status: summary.status,
            reason: summary.reason,
            retryable: summary.retryable,
            total_applications,
            total_records: total_applications,
            data_frame_count,
        };
        let start = encode_response(&ResponseEnvelope {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id,
            sequence: 0,
            snapshot_id: Some(snapshot_id),
            body: ResponseBody::UsageSummaryStart(Box::new(start)),
        })?;
        let end = encode_response(&ResponseEnvelope {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id,
            sequence,
            snapshot_id: Some(snapshot_id),
            body: ResponseBody::UsageSummaryEnd(end),
        })?;
        let mut encoded_frames = Vec::with_capacity(response_frames);
        encoded_frames.push(start);
        encoded_frames.extend(chunks);
        encoded_frames.push(end);
        validate_encoded_plan(&encoded_frames)?;
        Ok(Self { encoded_frames })
    }

    async fn write_to(self, stream: &mut UnixStream, deadline: Instant) -> Result<(), ServerError> {
        write_encoded_plan(self.encoded_frames, stream, deadline).await
    }
}

fn encode_record_chunks<T, F>(
    records: &[T],
    request_id: Uuid,
    snapshot_id: Uuid,
    sequence: &mut u32,
    encoded_chunks: &mut Vec<Vec<u8>>,
    body: F,
    record_kind: &'static str,
) -> Result<(), DaemonError>
where
    T: Clone,
    F: Fn(Vec<T>) -> ResponseBody,
{
    let mut offset = 0_usize;
    while offset < records.len() {
        let limit = (records.len() - offset).min(MAX_CHUNK_RECORDS);
        let mut selected = None;
        for count in 1..=limit {
            let response = ResponseEnvelope {
                protocol_version: WIRE_PROTOCOL_VERSION,
                request_id,
                sequence: *sequence,
                snapshot_id: Some(snapshot_id),
                body: body(records[offset..offset + count].to_vec()),
            };
            let encoded = serde_json::to_vec(&response).map_err(|_| {
                DaemonError::new(
                    "snapshot_serialization_failed",
                    format!("{record_kind}_chunk_serialization_failed"),
                    false,
                )
            })?;
            if encoded.len() > MAX_FRAME_PAYLOAD_BYTES {
                if count == 1 {
                    return Err(DaemonError::new(
                        "snapshot_record_oversize",
                        format!("single_{record_kind}_record_exceeds_frame_limit"),
                        false,
                    ));
                }
                break;
            }
            selected = Some((count, encoded));
        }
        let (selected_count, encoded) = selected.ok_or_else(|| {
            DaemonError::new(
                "snapshot_chunk_invalid",
                format!("{record_kind}_chunk_must_not_be_empty"),
                false,
            )
        })?;
        offset = offset.checked_add(selected_count).ok_or_else(|| {
            DaemonError::new("snapshot_counter_overflow", "record_offset_overflow", false)
        })?;
        encoded_chunks.push(encoded);
        if encoded_chunks
            .len()
            .checked_add(2)
            .is_none_or(|count| count > MAX_RESPONSE_FRAMES)
        {
            return Err(DaemonError::new(
                "snapshot_frame_limit",
                "snapshot_response_frames_exceeds_130",
                false,
            ));
        }
        *sequence = sequence.checked_add(1).ok_or_else(|| {
            DaemonError::new(
                "snapshot_sequence_overflow",
                "snapshot_sequence_overflow",
                false,
            )
        })?;
    }
    Ok(())
}

fn checked_u32(value: usize, name: &'static str) -> Result<u32, DaemonError> {
    u32::try_from(value).map_err(|_| {
        DaemonError::new(
            "snapshot_counter_overflow",
            format!("{name}_overflow"),
            false,
        )
    })
}

async fn write_encoded_plan(
    encoded_frames: Vec<Vec<u8>>,
    stream: &mut UnixStream,
    deadline: Instant,
) -> Result<(), ServerError> {
    for payload in encoded_frames {
        write_frame(stream, &payload, deadline).await?;
    }
    Ok(())
}

struct SnapshotPlan {
    encoded_frames: Vec<Vec<u8>>,
}

impl SnapshotPlan {
    fn build(request_id: Uuid, snapshot: TelemetrySnapshot) -> Result<Self, DaemonError> {
        if snapshot.schema_version != TELEMETRY_SCHEMA_VERSION {
            return Err(DaemonError::new(
                "snapshot_schema_unsupported",
                "snapshot_schema_must_be_3",
                false,
            ));
        }
        if snapshot.snapshot_id.is_nil() {
            return Err(DaemonError::new(
                "snapshot_identity_invalid",
                "snapshot_id_must_not_be_nil",
                false,
            ));
        }
        let total_applications = snapshot.applications.len();
        if total_applications > MAX_APPLICATION_RECORDS {
            return Err(DaemonError::new(
                "snapshot_application_limit",
                "snapshot_applications_exceeds_1024",
                false,
            ));
        }
        let total_records = total_applications;
        if total_records > MAX_TOTAL_RECORDS {
            return Err(DaemonError::new(
                "snapshot_record_limit",
                "snapshot_records_exceeds_4096",
                false,
            ));
        }

        let snapshot_id = snapshot.snapshot_id;
        let mut encoded_chunks = Vec::new();
        let mut offset = 0_usize;
        let mut sequence = 1_u32;
        while offset < snapshot.applications.len() {
            let (selected, encoded) = select_application_chunk(
                &snapshot.applications[offset..],
                request_id,
                snapshot_id,
                sequence,
            )?;
            offset = offset.checked_add(selected).ok_or_else(|| {
                DaemonError::new(
                    "snapshot_counter_overflow",
                    "application_offset_overflow",
                    false,
                )
            })?;
            encoded_chunks.push(encoded);
            let response_frames = encoded_chunks.len().checked_add(2).ok_or_else(|| {
                DaemonError::new(
                    "snapshot_frame_limit",
                    "snapshot_frame_count_overflow",
                    false,
                )
            })?;
            if response_frames > MAX_RESPONSE_FRAMES {
                return Err(DaemonError::new(
                    "snapshot_frame_limit",
                    "snapshot_response_frames_exceeds_130",
                    false,
                ));
            }
            sequence = sequence.checked_add(1).ok_or_else(|| {
                DaemonError::new(
                    "snapshot_sequence_overflow",
                    "snapshot_sequence_overflow",
                    false,
                )
            })?;
        }
        let data_frame_count = u32::try_from(encoded_chunks.len()).map_err(|_| {
            DaemonError::new(
                "snapshot_frame_limit",
                "snapshot_data_frame_count_overflow",
                false,
            )
        })?;
        let response_frames = encoded_chunks.len().checked_add(2).ok_or_else(|| {
            DaemonError::new(
                "snapshot_frame_limit",
                "snapshot_frame_count_overflow",
                false,
            )
        })?;
        if response_frames > MAX_RESPONSE_FRAMES {
            return Err(DaemonError::new(
                "snapshot_frame_limit",
                "snapshot_response_frames_exceeds_130",
                false,
            ));
        }

        let total_applications = u32::try_from(total_applications).map_err(|_| {
            DaemonError::new(
                "snapshot_record_limit",
                "snapshot_application_count_overflow",
                false,
            )
        })?;
        let total_records = u32::try_from(total_records).map_err(|_| {
            DaemonError::new(
                "snapshot_record_limit",
                "snapshot_record_count_overflow",
                false,
            )
        })?;
        let start = SnapshotStart {
            schema_version: snapshot.schema_version,
            captured_at_unix_ms: snapshot.captured_at_unix_ms,
            sample_interval_ms: snapshot.sample_interval_ms,
            logical_cpu_count: snapshot.logical_cpu_count,
            freshness: snapshot.freshness,
            status: snapshot.status,
            reason: snapshot.reason.clone(),
            retryable: snapshot.retryable,
            scope: snapshot.scope.clone(),
            last_success_at_unix_ms: snapshot.last_success_at_unix_ms,
            system_fd: snapshot.system_fd.clone(),
            total_applications,
            total_records,
            data_frame_count,
        };
        let end = SnapshotEnd {
            schema_version: snapshot.schema_version,
            freshness: snapshot.freshness,
            status: snapshot.status,
            reason: snapshot.reason,
            retryable: snapshot.retryable,
            scope: snapshot.scope,
            last_success_at_unix_ms: snapshot.last_success_at_unix_ms,
            total_applications,
            total_records,
            data_frame_count,
            permission_denied_counts: snapshot.permission_denied_counts,
            issues: snapshot.issues,
        };
        let start = encode_response(&ResponseEnvelope {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id,
            sequence: 0,
            snapshot_id: Some(snapshot_id),
            body: ResponseBody::SnapshotStart(Box::new(start)),
        })?;
        let end = encode_response(&ResponseEnvelope {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id,
            sequence,
            snapshot_id: Some(snapshot_id),
            body: ResponseBody::SnapshotEnd(end),
        })?;

        let mut encoded_frames = Vec::with_capacity(response_frames);
        encoded_frames.push(start);
        encoded_frames.extend(encoded_chunks);
        encoded_frames.push(end);
        validate_encoded_plan(&encoded_frames)?;
        Ok(Self { encoded_frames })
    }

    async fn write_to(self, stream: &mut UnixStream, deadline: Instant) -> Result<(), ServerError> {
        for payload in self.encoded_frames {
            write_frame(stream, &payload, deadline).await?;
        }
        Ok(())
    }
}

fn select_application_chunk(
    records: &[localdesk_domain::ApplicationSample],
    request_id: Uuid,
    snapshot_id: Uuid,
    sequence: u32,
) -> Result<(usize, Vec<u8>), DaemonError> {
    let limit = records.len().min(MAX_CHUNK_RECORDS);
    let mut selected = None;
    for count in 1..=limit {
        let response = ResponseEnvelope {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id,
            sequence,
            snapshot_id: Some(snapshot_id),
            body: ResponseBody::ApplicationChunk(ApplicationChunk {
                records: records[..count].to_vec(),
            }),
        };
        let encoded = serde_json::to_vec(&response).map_err(|_| {
            DaemonError::new(
                "snapshot_serialization_failed",
                "snapshot_chunk_serialization_failed",
                false,
            )
        })?;
        if encoded.len() > MAX_FRAME_PAYLOAD_BYTES {
            if count == 1 {
                return Err(DaemonError::new(
                    "snapshot_record_oversize",
                    "single_application_record_exceeds_frame_limit",
                    false,
                ));
            }
            break;
        }
        selected = Some((count, encoded));
    }
    selected.ok_or_else(|| {
        DaemonError::new(
            "snapshot_chunk_invalid",
            "snapshot_chunk_must_not_be_empty",
            false,
        )
    })
}

fn encode_response(response: &ResponseEnvelope) -> Result<Vec<u8>, DaemonError> {
    let encoded = serde_json::to_vec(response).map_err(|_| {
        DaemonError::new(
            "snapshot_serialization_failed",
            "snapshot_metadata_serialization_failed",
            false,
        )
    })?;
    if encoded.is_empty() || encoded.len() > MAX_FRAME_PAYLOAD_BYTES {
        return Err(DaemonError::new(
            "snapshot_metadata_oversize",
            "snapshot_metadata_exceeds_frame_limit",
            false,
        ));
    }
    Ok(encoded)
}

fn validate_encoded_plan(encoded_frames: &[Vec<u8>]) -> Result<(), DaemonError> {
    validate_encoded_plan_with_limits(encoded_frames, MAX_RESPONSE_FRAMES, MAX_RESPONSE_WIRE_BYTES)
}

fn validate_encoded_plan_with_limits(
    encoded_frames: &[Vec<u8>],
    max_frames: usize,
    max_wire_bytes: usize,
) -> Result<(), DaemonError> {
    if encoded_frames.len() > max_frames {
        return Err(DaemonError::new(
            "snapshot_frame_limit",
            "snapshot_response_frames_exceeds_130",
            false,
        ));
    }
    let mut wire_bytes = 0_usize;
    for payload in encoded_frames {
        if payload.is_empty() || payload.len() > MAX_FRAME_PAYLOAD_BYTES {
            return Err(DaemonError::new(
                "snapshot_frame_limit",
                "snapshot_frame_payload_invalid",
                false,
            ));
        }
        wire_bytes = wire_bytes
            .checked_add(4)
            .and_then(|total| total.checked_add(payload.len()))
            .ok_or_else(|| {
                DaemonError::new("snapshot_wire_limit", "snapshot_wire_bytes_overflow", false)
            })?;
        if wire_bytes > max_wire_bytes {
            return Err(DaemonError::new(
                "snapshot_wire_limit",
                "snapshot_wire_bytes_exceeds_limit",
                false,
            ));
        }
    }
    Ok(())
}

async fn write_response(
    stream: &mut UnixStream,
    response: &ResponseEnvelope,
    deadline: Instant,
) -> Result<(), ServerError> {
    let encoded = serde_json::to_vec(response).map_err(FrameError::InvalidJson)?;
    write_frame(stream, &encoded, deadline).await?;
    Ok(())
}

async fn serve_speedtest_stream(
    stream: &mut UnixStream,
    receiver: &mut mpsc::Receiver<SpeedTestStreamEvent>,
    request_id: Uuid,
    deadline: Instant,
) -> Result<(), ServerError> {
    let mut sequence = 0_u32;
    loop {
        let event = match timeout_at(deadline, receiver.recv()).await {
            Ok(Some(event)) => event,
            Ok(None) => return Err(ServerError::Protocol),
            Err(_) => return Err(ServerError::Connection(FrameError::DeadlineExceeded)),
        };
        let ended = matches!(event, SpeedTestStreamEvent::End(_));
        let body = match event {
            SpeedTestStreamEvent::Stage(stage) => {
                if !stage.validate() {
                    return Err(ServerError::Protocol);
                }
                ResponseBody::SpeedTestStage(stage)
            }
            SpeedTestStreamEvent::End(end) => {
                if !end.validate() {
                    return Err(ServerError::Protocol);
                }
                ResponseBody::SpeedTestBasicEnd(end)
            }
        };
        write_response(
            stream,
            &ResponseEnvelope {
                protocol_version: WIRE_PROTOCOL_VERSION,
                request_id,
                sequence,
                snapshot_id: None,
                body,
            },
            deadline,
        )
        .await?;
        sequence = sequence.checked_add(1).ok_or(ServerError::Protocol)?;
        if ended {
            return Ok(());
        }
    }
}

async fn serve_terminal_stream(
    stream: &mut UnixStream,
    provider: TerminalProvider,
    request_id: Uuid,
    session_id: localdesk_remote_core::TerminalSessionId,
    max_bytes: u32,
) -> Result<(), ServerError> {
    let initial_status = terminal_stream_status(&provider, session_id).await?;
    let mut sequence = 0_u32;
    write_response(
        stream,
        &ResponseEnvelope {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id,
            sequence,
            snapshot_id: None,
            body: ResponseBody::TerminalStreamStart(TerminalStreamStart {
                session_id,
                max_bytes,
                status: initial_status.clone(),
            }),
        },
        Instant::now() + SNAPSHOT_TOTAL_DEADLINE,
    )
    .await?;
    sequence = sequence.checked_add(1).ok_or(ServerError::Protocol)?;

    let mut last_status = initial_status.clone();
    let mut final_status =
        (!matches!(initial_status.state, TerminalState::Running)).then_some(initial_status);
    let mut next_status_at = Instant::now() + TERMINAL_STREAM_STATUS_INTERVAL;
    let mut last_status_sent_at = Instant::now();

    loop {
        let read_command = TerminalCommand::Read {
            session_id,
            max_bytes,
        };
        let result = terminal_stream_command(&provider, read_command.clone()).await?;
        if result.validate_for(&read_command).is_err() {
            return Err(ServerError::Protocol);
        }
        let TerminalResult::Read { output, .. } = result else {
            return Err(ServerError::Protocol);
        };

        match output {
            TerminalRead::Data(data) => {
                write_response(
                    stream,
                    &ResponseEnvelope {
                        protocol_version: WIRE_PROTOCOL_VERSION,
                        request_id,
                        sequence,
                        snapshot_id: None,
                        body: ResponseBody::TerminalStreamData(TerminalStreamData {
                            session_id,
                            data,
                        }),
                    },
                    Instant::now() + SNAPSHOT_TOTAL_DEADLINE,
                )
                .await?;
                sequence = sequence.checked_add(1).ok_or(ServerError::Protocol)?;
            }
            TerminalRead::Pending if final_status.is_some() => {
                let status = final_status
                    .clone()
                    .expect("terminal final status is present");
                write_terminal_stream_end(stream, request_id, sequence, session_id, status).await?;
                return Ok(());
            }
            TerminalRead::Pending => sleep(TERMINAL_STREAM_IDLE_INTERVAL).await,
            TerminalRead::EndOfStream => {
                let status = match &final_status {
                    Some(status) => status.clone(),
                    None => terminal_stream_status(&provider, session_id).await?,
                };
                if matches!(status.state, TerminalState::Running) {
                    sleep(TERMINAL_STREAM_IDLE_INTERVAL).await;
                } else {
                    write_terminal_stream_end(stream, request_id, sequence, session_id, status)
                        .await?;
                    return Ok(());
                }
            }
        }

        if final_status.is_none() && Instant::now() >= next_status_at {
            let status = terminal_stream_status(&provider, session_id).await?;
            next_status_at = Instant::now() + TERMINAL_STREAM_STATUS_INTERVAL;
            if status != last_status
                || Instant::now().duration_since(last_status_sent_at)
                    >= TERMINAL_STREAM_HEARTBEAT_INTERVAL
            {
                write_response(
                    stream,
                    &ResponseEnvelope {
                        protocol_version: WIRE_PROTOCOL_VERSION,
                        request_id,
                        sequence,
                        snapshot_id: None,
                        body: ResponseBody::TerminalStreamStatus(TerminalStreamStatus {
                            session_id,
                            status: status.clone(),
                        }),
                    },
                    Instant::now() + SNAPSHOT_TOTAL_DEADLINE,
                )
                .await?;
                sequence = sequence.checked_add(1).ok_or(ServerError::Protocol)?;
                last_status = status.clone();
                last_status_sent_at = Instant::now();
            }
            if !matches!(status.state, TerminalState::Running) {
                final_status = Some(status);
            }
        }
    }
}

async fn terminal_stream_command(
    provider: &TerminalProvider,
    command: TerminalCommand,
) -> Result<TerminalResult, ServerError> {
    match timeout_at(Instant::now() + SNAPSHOT_TOTAL_DEADLINE, provider(command)).await {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(_)) => Err(ServerError::Protocol),
        Err(_) => Err(ServerError::Connection(FrameError::DeadlineExceeded)),
    }
}

async fn terminal_stream_status(
    provider: &TerminalProvider,
    session_id: localdesk_remote_core::TerminalSessionId,
) -> Result<localdesk_remote_core::TerminalStatus, ServerError> {
    let command = TerminalCommand::Poll { session_id };
    let result = terminal_stream_command(provider, command.clone()).await?;
    if result.validate_for(&command).is_err() {
        return Err(ServerError::Protocol);
    }
    let TerminalResult::Status { status, .. } = result else {
        return Err(ServerError::Protocol);
    };
    Ok(status)
}

async fn write_terminal_stream_end(
    stream: &mut UnixStream,
    request_id: Uuid,
    sequence: u32,
    session_id: localdesk_remote_core::TerminalSessionId,
    status: localdesk_remote_core::TerminalStatus,
) -> Result<(), ServerError> {
    write_response(
        stream,
        &ResponseEnvelope {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id,
            sequence,
            snapshot_id: None,
            body: ResponseBody::TerminalStreamEnd(TerminalStreamEnd { session_id, status }),
        },
        Instant::now() + SNAPSHOT_TOTAL_DEADLINE,
    )
    .await
}

async fn write_terminal_error(
    stream: &mut UnixStream,
    request_id: Uuid,
    deadline: Instant,
    error: DaemonError,
) -> Result<(), ServerError> {
    let response = ResponseEnvelope::terminal_error(request_id, 0, None, error);
    match write_response(stream, &response, deadline).await {
        Ok(()) => Ok(()),
        Err(ServerError::Connection(FrameError::Oversize)) => {
            let fallback = ResponseEnvelope::terminal_error(
                request_id,
                0,
                None,
                DaemonError::new(
                    "daemon_error_oversize",
                    "daemon_error_exceeds_frame_limit",
                    false,
                ),
            );
            write_response(stream, &fallback, deadline).await
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoded_plan_rejects_frame_count_overflow() {
        let frames = vec![vec![0_u8; MAX_FRAME_PAYLOAD_BYTES]; MAX_RESPONSE_FRAMES];
        assert!(validate_encoded_plan(&frames).is_ok());
        let mut frames = frames;
        frames.push(vec![1]);
        assert_eq!(
            validate_encoded_plan(&frames)
                .expect_err("frame limit")
                .code,
            "snapshot_frame_limit"
        );
    }
}
