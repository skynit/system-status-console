use localdesk_domain::{
    APPD_HEALTH_CAPABILITY, ApplicationSample, Capability, HealthState, IssueCount,
    MAX_NOTE_EXPORT_DATA_FRAMES, NetworkApplicationTraffic, NetworkCapabilityState,
    NetworkCoverage, NetworkFreshness, NetworkInterfaceSample, NetworkRate, NetworkTrafficTotals,
    NoteExportFormat, NoteMutationResult, NoteQuery, NoteSummary, NotesCommand, SystemFdSample,
    TelemetryFreshness, TelemetryStatus, UsageApplicationDuration, UsageCoverage,
    UsageSummaryQuery,
};
use localdesk_remote_core::{
    RemoteAdapterCatalog, RemoteProfileCommand, RemoteProfileResult, RemoteSessionCommand,
    RemoteSessionResult, SecretCommand, SecretCommandResult, TerminalCommand, TerminalData,
    TerminalResult, TerminalSessionId, TerminalStatus,
};
use localdesk_transfers::{
    TransferCommand, TransferLocalHandleGrant, TransferLocalHandlePurpose, TransferQuery,
    TransferTask,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;
use uuid::Uuid;

pub const WIRE_PROTOCOL_VERSION: u16 = 12;
pub const MAX_REQUESTED_CAPABILITIES: usize = 32;
pub const MAX_CHUNK_RECORDS: usize = 32;
pub const MAX_APPLICATION_RECORDS: usize = 1_024;
pub const MAX_TOTAL_RECORDS: usize = 4_096;
pub const MAX_RESPONSE_FRAMES: usize = 130;
pub const MAX_RESPONSE_WIRE_BYTES: usize = 9 * 1_024 * 1_024;
pub const MAX_NOTES_EXPORT_FRAMES: usize = MAX_NOTE_EXPORT_DATA_FRAMES + 2;
pub const MAX_NOTES_EXPORT_WIRE_BYTES: usize = 40 * 1_024 * 1_024;
pub const MAX_TRANSFER_BIND_PATH_BYTES: usize = 4 * 1024;

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RequestEnvelope {
    pub protocol_version: u16,
    pub request_id: Uuid,
    pub body: RequestBody,
}

impl RequestEnvelope {
    pub fn health(client_version: impl Into<String>, requested_capabilities: Vec<String>) -> Self {
        Self {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id: Uuid::new_v4(),
            body: RequestBody::Health(HealthRequest {
                client_version: client_version.into(),
                requested_capabilities,
            }),
        }
    }

    pub fn appd_health(client_version: impl Into<String>) -> Self {
        Self::health(client_version, vec![APPD_HEALTH_CAPABILITY.to_owned()])
    }

    pub fn telemetry_snapshot() -> Self {
        Self {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id: Uuid::new_v4(),
            body: RequestBody::TelemetrySnapshot(TelemetrySnapshotRequest {}),
        }
    }

    pub fn network_snapshot() -> Self {
        Self {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id: Uuid::new_v4(),
            body: RequestBody::NetworkSnapshot(NetworkSnapshotRequest {}),
        }
    }

    pub fn usage_summary(query: UsageSummaryQuery) -> Self {
        Self {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id: Uuid::new_v4(),
            body: RequestBody::UsageSummary(UsageSummaryRequest { query }),
        }
    }

    pub fn remote_capabilities() -> Self {
        Self {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id: Uuid::new_v4(),
            body: RequestBody::RemoteCapabilities(RemoteCapabilitiesRequest {}),
        }
    }

    pub fn remote_profile(command: RemoteProfileCommand) -> Self {
        Self {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id: Uuid::new_v4(),
            body: RequestBody::RemoteProfile(command),
        }
    }

    pub fn secret(command: SecretCommand) -> Self {
        Self {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id: Uuid::new_v4(),
            body: RequestBody::Secret(command),
        }
    }

    pub fn remote_session(command: RemoteSessionCommand) -> Self {
        Self {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id: Uuid::new_v4(),
            body: RequestBody::RemoteSession(command),
        }
    }

    pub fn notes(command: NotesCommand) -> Self {
        Self {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id: Uuid::new_v4(),
            body: RequestBody::Notes(command),
        }
    }

    pub fn terminal(command: TerminalCommand) -> Self {
        Self {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id: Uuid::new_v4(),
            body: RequestBody::Terminal(command),
        }
    }

    pub fn transfer(command: TransferCommand) -> Self {
        Self {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id: Uuid::new_v4(),
            body: RequestBody::Transfer(command),
        }
    }

    pub fn transfer_local_handle(bind: TransferLocalHandleBind) -> Self {
        Self {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id: Uuid::new_v4(),
            body: RequestBody::TransferLocalHandle(bind),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum RequestBody {
    Health(HealthRequest),
    TelemetrySnapshot(TelemetrySnapshotRequest),
    NetworkSnapshot(NetworkSnapshotRequest),
    UsageSummary(UsageSummaryRequest),
    RemoteCapabilities(RemoteCapabilitiesRequest),
    RemoteProfile(RemoteProfileCommand),
    Secret(SecretCommand),
    RemoteSession(RemoteSessionCommand),
    Notes(NotesCommand),
    Terminal(TerminalCommand),
    Transfer(TransferCommand),
    TransferLocalHandle(TransferLocalHandleBind),
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransferLocalHandleBind {
    pub purpose: TransferLocalHandlePurpose,
    pub path: PathBuf,
}

impl TransferLocalHandleBind {
    pub fn validate(&self) -> bool {
        validate_bind_path(&self.path)
    }
}

fn validate_bind_path(path: &Path) -> bool {
    path.is_absolute()
        && path.to_str().is_some_and(|value| {
            !value.is_empty()
                && value.len() <= MAX_TRANSFER_BIND_PATH_BYTES
                && !value.chars().any(char::is_control)
        })
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HealthRequest {
    pub client_version: String,
    pub requested_capabilities: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TelemetrySnapshotRequest {}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSnapshotRequest {}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UsageSummaryRequest {
    pub query: UsageSummaryQuery,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteCapabilitiesRequest {}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseEnvelope {
    pub protocol_version: u16,
    pub request_id: Uuid,
    pub sequence: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<Uuid>,
    pub body: ResponseBody,
}

impl ResponseEnvelope {
    pub fn terminal_error(
        request_id: Uuid,
        sequence: u32,
        snapshot_id: Option<Uuid>,
        error: DaemonError,
    ) -> Self {
        Self {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id,
            sequence,
            snapshot_id,
            body: ResponseBody::Error(error),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalStreamStart {
    pub session_id: TerminalSessionId,
    pub max_bytes: u32,
    pub status: TerminalStatus,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalStreamData {
    pub session_id: TerminalSessionId,
    pub data: TerminalData,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalStreamStatus {
    pub session_id: TerminalSessionId,
    pub status: TerminalStatus,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalStreamEnd {
    pub session_id: TerminalSessionId,
    pub status: TerminalStatus,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum ResponseBody {
    HealthReport(HealthReport),
    SnapshotStart(Box<SnapshotStart>),
    ApplicationChunk(ApplicationChunk),
    SnapshotEnd(SnapshotEnd),
    NetworkSnapshotStart(Box<NetworkSnapshotStart>),
    NetworkInterfaceChunk(NetworkInterfaceChunk),
    NetworkApplicationChunk(NetworkApplicationChunk),
    NetworkSnapshotEnd(NetworkSnapshotEnd),
    UsageSummaryStart(Box<UsageSummaryStart>),
    UsageApplicationChunk(UsageApplicationChunk),
    UsageSummaryEnd(UsageSummaryEnd),
    RemoteCapabilities(RemoteAdapterCatalog),
    RemoteProfile(RemoteProfileResult),
    Secret(SecretCommandResult),
    RemoteSession(RemoteSessionResult),
    TerminalStreamStart(TerminalStreamStart),
    TerminalStreamData(TerminalStreamData),
    TerminalStreamStatus(TerminalStreamStatus),
    TerminalStreamEnd(TerminalStreamEnd),
    NotesMutation(NoteMutationResult),
    NotesPageStart(Box<NotesPageStart>),
    NoteSummaryChunk(NoteSummaryChunk),
    NotesPageEnd(NotesPageEnd),
    NotesContentStart(Box<NotesContentStart>),
    NotesContentChunk(NotesContentChunk),
    NotesContentEnd(NotesContentEnd),
    Terminal(TerminalResult),
    Transfer(Box<localdesk_transfers::TransferOutput>),
    TransferPageStart(Box<TransferPageStart>),
    TransferTaskChunk(TransferTaskChunk),
    TransferPageEnd(TransferPageEnd),
    TransferLocalHandle(TransferLocalHandleGrant),
    Error(DaemonError),
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransferPageStart {
    pub schema_version: u16,
    pub query: TransferQuery,
    pub total_tasks: u32,
    pub data_frame_count: u32,
    pub has_more: bool,
    pub next_offset: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransferTaskChunk {
    pub records: Vec<TransferTask>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransferPageEnd {
    pub schema_version: u16,
    pub query: TransferQuery,
    pub total_tasks: u32,
    pub data_frame_count: u32,
    pub has_more: bool,
    pub next_offset: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NotesPageStart {
    pub schema_version: u16,
    pub query: NoteQuery,
    pub total_notes: u32,
    pub data_frame_count: u32,
    pub has_more: bool,
    pub next_offset: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NoteSummaryChunk {
    pub records: Vec<NoteSummary>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NotesPageEnd {
    pub schema_version: u16,
    pub query: NoteQuery,
    pub total_notes: u32,
    pub data_frame_count: u32,
    pub has_more: bool,
    pub next_offset: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NotesContentKind {
    Document,
    Export { format: NoteExportFormat },
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NotesContentStart {
    pub schema_version: u16,
    pub kind: NotesContentKind,
    pub note: Option<NoteSummary>,
    pub total_bytes: u32,
    pub content_sha256: String,
    pub data_frame_count: u32,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NotesContentChunk {
    pub offset: u32,
    pub raw_bytes: u32,
    pub data_base64: String,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NotesContentEnd {
    pub schema_version: u16,
    pub kind: NotesContentKind,
    pub total_bytes: u32,
    pub content_sha256: String,
    pub data_frame_count: u32,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HealthReport {
    pub daemon_version: String,
    pub health: HealthState,
    pub reason: String,
    pub capabilities: Vec<Capability>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotStart {
    pub schema_version: u16,
    pub captured_at_unix_ms: Option<i64>,
    pub sample_interval_ms: Option<u64>,
    pub logical_cpu_count: Option<u32>,
    pub freshness: TelemetryFreshness,
    pub status: TelemetryStatus,
    pub reason: String,
    pub retryable: bool,
    pub scope: String,
    pub last_success_at_unix_ms: Option<i64>,
    pub system_fd: SystemFdSample,
    pub total_applications: u32,
    pub total_records: u32,
    pub data_frame_count: u32,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationChunk {
    pub records: Vec<ApplicationSample>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotEnd {
    pub schema_version: u16,
    pub freshness: TelemetryFreshness,
    pub status: TelemetryStatus,
    pub reason: String,
    pub retryable: bool,
    pub scope: String,
    pub last_success_at_unix_ms: Option<i64>,
    pub total_applications: u32,
    pub total_records: u32,
    pub data_frame_count: u32,
    pub permission_denied_counts: Vec<IssueCount>,
    pub issues: Vec<IssueCount>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSnapshotStart {
    pub schema_version: u16,
    pub captured_at_unix_ms: Option<i64>,
    pub observed_boottime_ms: Option<u64>,
    pub sample_interval_ms: Option<u64>,
    pub last_success_at_unix_ms: Option<i64>,
    pub freshness: NetworkFreshness,
    pub retryable: bool,
    pub system_traffic: NetworkCapabilityState,
    pub per_application: NetworkCapabilityState,
    pub coverage: NetworkCoverage,
    pub totals: Option<NetworkTrafficTotals>,
    pub aggregate_rate: NetworkRate,
    pub total_interfaces: u32,
    pub total_applications: u32,
    pub total_records: u32,
    pub data_frame_count: u32,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkInterfaceChunk {
    pub records: Vec<NetworkInterfaceSample>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkApplicationChunk {
    pub records: Vec<NetworkApplicationTraffic>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSnapshotEnd {
    pub schema_version: u16,
    pub total_interfaces: u32,
    pub total_applications: u32,
    pub total_records: u32,
    pub data_frame_count: u32,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UsageSummaryStart {
    pub schema_version: u16,
    pub captured_at_unix_ms: Option<i64>,
    pub query: UsageSummaryQuery,
    pub status: localdesk_domain::CapabilityAvailability,
    pub reason: String,
    pub retryable: bool,
    pub coverage: UsageCoverage,
    pub total_applications: u32,
    pub total_records: u32,
    pub data_frame_count: u32,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UsageApplicationChunk {
    pub records: Vec<UsageApplicationDuration>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UsageSummaryEnd {
    pub schema_version: u16,
    pub query: UsageSummaryQuery,
    pub status: localdesk_domain::CapabilityAvailability,
    pub reason: String,
    pub retryable: bool,
    pub total_applications: u32,
    pub total_records: u32,
    pub data_frame_count: u32,
}

#[derive(Debug, Clone, Deserialize, Eq, Error, PartialEq, Serialize)]
#[error("daemon returned {code}: {reason}")]
#[serde(deny_unknown_fields)]
pub struct DaemonError {
    pub code: String,
    pub reason: String,
    pub retryable: bool,
}

impl DaemonError {
    pub fn new(code: impl Into<String>, reason: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: code.into(),
            reason: reason.into(),
            retryable,
        }
    }
}
