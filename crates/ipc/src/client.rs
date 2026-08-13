use crate::{
    frame::{FrameError, WireBudget, read_json, read_json_with_idle_timeout, write_json},
    message::{
        DaemonError, HealthReport, MAX_APPLICATION_RECORDS, MAX_CHUNK_RECORDS,
        MAX_NOTES_EXPORT_FRAMES, MAX_NOTES_EXPORT_WIRE_BYTES, MAX_RESPONSE_FRAMES,
        MAX_RESPONSE_WIRE_BYTES, MAX_TOTAL_RECORDS, NetworkSnapshotEnd, NetworkSnapshotStart,
        NotesContentEnd, NotesContentKind, NotesContentStart, NotesPageEnd, NotesPageStart,
        RequestBody, RequestEnvelope, ResponseBody, ResponseEnvelope, SnapshotEnd, SnapshotStart,
        TerminalStreamData, TerminalStreamEnd, TerminalStreamStart, TerminalStreamStatus,
        TransferPageEnd, TransferPageStart, UsageSummaryEnd, UsageSummaryStart,
        WIRE_PROTOCOL_VERSION,
    },
    peer::{PeerError, verify_peer_uid},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use localdesk_domain::{
    MAX_NETWORK_APPLICATIONS, MAX_NETWORK_INTERFACES, MAX_NOTE_BODY_BYTES,
    MAX_NOTE_CONTENT_BASE64_BYTES, MAX_NOTE_EXPORT_BYTES, MAX_USAGE_APPLICATIONS,
    NETWORK_SCHEMA_VERSION, NOTE_CONTENT_CHUNK_BYTES, NOTES_SCHEMA_VERSION, NetworkSnapshot,
    NoteDocument, NoteExport, NoteMutationResult, NotePage, NotesCommand, NotesOutput,
    TELEMETRY_SCHEMA_VERSION, TelemetrySnapshot, USAGE_SCHEMA_VERSION, UsageSummary,
    UsageSummaryQuery,
};
use localdesk_remote_core::{
    RemoteAdapterCatalog, RemoteProfileResult, RemoteSessionResult, SecretCommandResult,
    TerminalCommand, TerminalData, TerminalResult, TerminalSessionId, TerminalState,
    TerminalStatus,
};
use localdesk_transfers::{
    MAX_TRANSFER_PAGE_TASKS, TRANSFER_PUBLIC_SCHEMA_VERSION, TransferCommand,
    TransferLocalHandleGrant, TransferOutput, TransferPage, TransferTask,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{io, path::Path, time::Duration};
use thiserror::Error;
use tokio::{
    io::AsyncReadExt,
    net::UnixStream,
    time::{Instant, timeout_at},
};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum TerminalStreamEvent {
    Started {
        session_id: TerminalSessionId,
        max_bytes: u32,
        status: TerminalStatus,
    },
    Data {
        session_id: TerminalSessionId,
        data: TerminalData,
    },
    Status {
        session_id: TerminalSessionId,
        status: TerminalStatus,
    },
    Ended {
        session_id: TerminalSessionId,
        status: TerminalStatus,
    },
}

pub const HEALTH_TOTAL_DEADLINE: Duration = Duration::from_secs(2);
pub const SNAPSHOT_TOTAL_DEADLINE: Duration = Duration::from_secs(5);
pub const REMOTE_SESSION_TOTAL_DEADLINE: Duration = Duration::from_secs(70);

#[derive(Debug, Error)]
pub enum ClientError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    Daemon(DaemonError),
}

impl ClientError {
    pub const fn reason_code(&self) -> &'static str {
        match self {
            Self::Transport(error) => error.reason_code(),
            Self::Protocol(error) => error.reason_code(),
            Self::Daemon(_) => "appd_daemon_error",
        }
    }
}

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("appd operation timed out")]
    Timeout,
    #[error("appd I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("appd peer verification failed: {0}")]
    Peer(#[from] PeerError),
    #[error("appd frame transport failed: {0}")]
    Frame(FrameError),
}

impl TransportError {
    pub const fn reason_code(&self) -> &'static str {
        match self {
            Self::Timeout => "appd_timeout",
            Self::Io(_) => "appd_connection_failed",
            Self::Peer(_) => "appd_peer_rejected",
            Self::Frame(FrameError::IdleTimeout | FrameError::DeadlineExceeded) => "appd_timeout",
            Self::Frame(_) => "appd_transport_failed",
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, Error, PartialEq)]
pub enum ProtocolError {
    #[error("request body is not valid for the client operation")]
    RequestBody,
    #[error("response protocol version did not match v12")]
    Version,
    #[error("response request_id did not match")]
    RequestId,
    #[error("response sequence was invalid")]
    Sequence,
    #[error("response snapshot identity was invalid")]
    SnapshotIdentity,
    #[error("response body was invalid for the stream state")]
    UnexpectedBody,
    #[error("response frame was malformed or exceeded a hard limit")]
    InvalidFrame,
    #[error("snapshot Start declarations were invalid")]
    InvalidStart,
    #[error("snapshot chunk records were invalid")]
    InvalidChunk,
    #[error("snapshot End did not exactly match Start and received records")]
    InvalidEnd,
    #[error("snapshot stream ended without a terminal response")]
    MissingTerminal,
    #[error("terminal response was followed by trailing bytes")]
    TrailingData,
    #[error("response sequence arithmetic overflowed")]
    SequenceOverflow,
}

impl ProtocolError {
    pub const fn reason_code(self) -> &'static str {
        match self {
            Self::RequestBody => "appd_request_body_invalid",
            Self::Version => "appd_protocol_version_mismatch",
            Self::RequestId => "appd_request_id_mismatch",
            Self::Sequence => "appd_sequence_invalid",
            Self::SnapshotIdentity => "snapshot_identity_mismatch",
            Self::UnexpectedBody => "appd_response_body_invalid",
            Self::InvalidFrame => "appd_invalid_response_frame",
            Self::InvalidStart => "snapshot_start_invalid",
            Self::InvalidChunk => "snapshot_chunk_invalid",
            Self::InvalidEnd => "snapshot_end_invalid",
            Self::MissingTerminal => "snapshot_terminal_missing",
            Self::TrailingData => "appd_trailing_response_data",
            Self::SequenceOverflow => "appd_sequence_overflow",
        }
    }
}

pub async fn request_health(
    path: &Path,
    request: RequestEnvelope,
) -> Result<HealthReport, ClientError> {
    let RequestBody::Health(health_request) = &request.body else {
        return Err(ProtocolError::RequestBody.into());
    };
    let requested_capabilities = health_request.requested_capabilities.clone();
    let request_id = request.request_id;
    let deadline = Instant::now() + HEALTH_TOTAL_DEADLINE;
    let mut stream = connect(path, deadline).await?;
    write_json(&mut stream, &request, deadline)
        .await
        .map_err(map_frame_error)?;

    let mut budget = response_budget();
    let response = read_response(&mut stream, deadline, &mut budget, false).await?;
    validate_envelope(&response, request_id)?;
    if response.sequence != 0 {
        return Err(ProtocolError::Sequence.into());
    }
    if response.snapshot_id.is_some() {
        return Err(ProtocolError::SnapshotIdentity.into());
    }

    match response.body {
        ResponseBody::HealthReport(report) => {
            if report.health == localdesk_domain::HealthState::Unreachable
                || report.capabilities.len() != requested_capabilities.len()
                || report
                    .capabilities
                    .iter()
                    .zip(requested_capabilities)
                    .any(|(capability, requested)| capability.id != requested)
            {
                return Err(ProtocolError::UnexpectedBody.into());
            }
            expect_eof(&mut stream, deadline).await?;
            Ok(report)
        }
        ResponseBody::Error(error) => {
            expect_eof(&mut stream, deadline).await?;
            Err(ClientError::Daemon(error))
        }
        _ => Err(ProtocolError::UnexpectedBody.into()),
    }
}

pub async fn request_telemetry_snapshot(
    path: &Path,
    request: RequestEnvelope,
) -> Result<TelemetrySnapshot, ClientError> {
    if !matches!(request.body, RequestBody::TelemetrySnapshot(_)) {
        return Err(ProtocolError::RequestBody.into());
    }
    let request_id = request.request_id;
    let deadline = Instant::now() + SNAPSHOT_TOTAL_DEADLINE;
    let mut stream = connect(path, deadline).await?;
    write_json(&mut stream, &request, deadline)
        .await
        .map_err(map_frame_error)?;

    let mut budget = response_budget();
    let response = read_response(&mut stream, deadline, &mut budget, false).await?;
    validate_envelope(&response, request_id)?;
    if response.sequence != 0 {
        return Err(ProtocolError::Sequence.into());
    }
    let snapshot_id = response
        .snapshot_id
        .filter(|snapshot_id| !snapshot_id.is_nil());

    let start = match response.body {
        ResponseBody::SnapshotStart(start) => {
            if snapshot_id.is_none() {
                return Err(ProtocolError::SnapshotIdentity.into());
            }
            validate_start(&start)?;
            start
        }
        ResponseBody::Error(error) => {
            if response.snapshot_id.is_some() {
                return Err(ProtocolError::SnapshotIdentity.into());
            }
            expect_eof(&mut stream, deadline).await?;
            return Err(ClientError::Daemon(error));
        }
        _ => return Err(ProtocolError::UnexpectedBody.into()),
    };
    let snapshot_id = snapshot_id.expect("validated snapshot identity");
    let mut expected_sequence = next_sequence(0)?;
    let mut data_frames = 0_u32;
    let mut total_records = 0_usize;
    let mut applications = Vec::with_capacity(start.total_applications as usize);

    loop {
        let response = read_response(&mut stream, deadline, &mut budget, true).await?;
        validate_envelope(&response, request_id)?;
        if response.sequence != expected_sequence {
            return Err(ProtocolError::Sequence.into());
        }
        if response.snapshot_id != Some(snapshot_id) {
            return Err(ProtocolError::SnapshotIdentity.into());
        }

        match response.body {
            ResponseBody::ApplicationChunk(chunk) => {
                let record_count = chunk.records.len();
                if record_count == 0 || record_count > MAX_CHUNK_RECORDS {
                    return Err(ProtocolError::InvalidChunk.into());
                }
                data_frames = data_frames
                    .checked_add(1)
                    .ok_or(ProtocolError::SequenceOverflow)?;
                if data_frames > start.data_frame_count {
                    return Err(ProtocolError::InvalidChunk.into());
                }
                total_records = total_records
                    .checked_add(record_count)
                    .ok_or(ProtocolError::InvalidChunk)?;
                let application_count = applications
                    .len()
                    .checked_add(record_count)
                    .ok_or(ProtocolError::InvalidChunk)?;
                if total_records > start.total_records as usize
                    || total_records > MAX_TOTAL_RECORDS
                    || application_count > start.total_applications as usize
                {
                    return Err(ProtocolError::InvalidChunk.into());
                }
                applications.extend(chunk.records);
                expected_sequence = next_sequence(expected_sequence)?;
            }
            ResponseBody::SnapshotEnd(end) => {
                validate_end(&start, &end, data_frames, total_records, applications.len())?;
                expect_eof(&mut stream, deadline).await?;
                return Ok(TelemetrySnapshot {
                    schema_version: start.schema_version,
                    snapshot_id,
                    captured_at_unix_ms: start.captured_at_unix_ms,
                    sample_interval_ms: start.sample_interval_ms,
                    logical_cpu_count: start.logical_cpu_count,
                    freshness: start.freshness,
                    status: start.status,
                    reason: start.reason,
                    retryable: start.retryable,
                    scope: start.scope,
                    last_success_at_unix_ms: start.last_success_at_unix_ms,
                    permission_denied_counts: end.permission_denied_counts,
                    issues: end.issues,
                    system_fd: start.system_fd,
                    applications,
                });
            }
            ResponseBody::Error(error) => {
                expect_eof(&mut stream, deadline).await?;
                return Err(ClientError::Daemon(error));
            }
            _ => return Err(ProtocolError::UnexpectedBody.into()),
        }
    }
}

pub async fn request_remote_capabilities(
    path: &Path,
    request: RequestEnvelope,
) -> Result<RemoteAdapterCatalog, ClientError> {
    if !matches!(request.body, RequestBody::RemoteCapabilities(_)) {
        return Err(ProtocolError::RequestBody.into());
    }
    let request_id = request.request_id;
    let deadline = Instant::now() + HEALTH_TOTAL_DEADLINE;
    let mut stream = connect(path, deadline).await?;
    write_json(&mut stream, &request, deadline)
        .await
        .map_err(map_frame_error)?;
    let mut budget = response_budget();
    let response = read_response(&mut stream, deadline, &mut budget, false).await?;
    validate_envelope(&response, request_id)?;
    if response.sequence != 0 || response.snapshot_id.is_some() {
        return Err(ProtocolError::UnexpectedBody.into());
    }
    match response.body {
        ResponseBody::RemoteCapabilities(catalog) => {
            catalog
                .validate()
                .map_err(|_| ProtocolError::UnexpectedBody)?;
            expect_eof(&mut stream, deadline).await?;
            Ok(catalog)
        }
        ResponseBody::Error(error) => {
            expect_eof(&mut stream, deadline).await?;
            Err(ClientError::Daemon(error))
        }
        _ => Err(ProtocolError::UnexpectedBody.into()),
    }
}

pub async fn request_remote_profile(
    path: &Path,
    request: RequestEnvelope,
) -> Result<RemoteProfileResult, ClientError> {
    let RequestBody::RemoteProfile(command) = &request.body else {
        return Err(ProtocolError::RequestBody.into());
    };
    command.validate().map_err(|_| ProtocolError::RequestBody)?;
    let command = command.clone();
    let request_id = request.request_id;
    let deadline = Instant::now() + HEALTH_TOTAL_DEADLINE;
    let mut stream = connect(path, deadline).await?;
    write_json(&mut stream, &request, deadline)
        .await
        .map_err(map_frame_error)?;
    let mut budget = response_budget();
    let response = read_response(&mut stream, deadline, &mut budget, false).await?;
    validate_envelope(&response, request_id)?;
    if response.sequence != 0 || response.snapshot_id.is_some() {
        return Err(ProtocolError::UnexpectedBody.into());
    }
    match response.body {
        ResponseBody::RemoteProfile(result) => {
            result
                .validate_for(&command)
                .map_err(|_| ProtocolError::UnexpectedBody)?;
            expect_eof(&mut stream, deadline).await?;
            Ok(result)
        }
        ResponseBody::Error(error) => {
            expect_eof(&mut stream, deadline).await?;
            Err(ClientError::Daemon(error))
        }
        _ => Err(ProtocolError::UnexpectedBody.into()),
    }
}

pub async fn request_secret(
    path: &Path,
    request: RequestEnvelope,
) -> Result<SecretCommandResult, ClientError> {
    let RequestBody::Secret(command) = &request.body else {
        return Err(ProtocolError::RequestBody.into());
    };
    command.validate().map_err(|_| ProtocolError::RequestBody)?;
    let command = command.clone();
    let request_id = request.request_id;
    let deadline = Instant::now() + HEALTH_TOTAL_DEADLINE;
    let mut stream = connect(path, deadline).await?;
    write_json(&mut stream, &request, deadline)
        .await
        .map_err(map_frame_error)?;
    let mut budget = response_budget();
    let response = read_response(&mut stream, deadline, &mut budget, false).await?;
    validate_envelope(&response, request_id)?;
    if response.sequence != 0 || response.snapshot_id.is_some() {
        return Err(ProtocolError::UnexpectedBody.into());
    }
    match response.body {
        ResponseBody::Secret(result) => {
            result
                .validate_for(&command)
                .map_err(|_| ProtocolError::UnexpectedBody)?;
            expect_eof(&mut stream, deadline).await?;
            Ok(result)
        }
        ResponseBody::Error(error) => {
            expect_eof(&mut stream, deadline).await?;
            Err(ClientError::Daemon(error))
        }
        _ => Err(ProtocolError::UnexpectedBody.into()),
    }
}

pub async fn request_remote_session(
    path: &Path,
    request: RequestEnvelope,
) -> Result<RemoteSessionResult, ClientError> {
    let RequestBody::RemoteSession(command) = &request.body else {
        return Err(ProtocolError::RequestBody.into());
    };
    command.validate().map_err(|_| ProtocolError::RequestBody)?;
    let command = command.clone();
    let request_id = request.request_id;
    let deadline = Instant::now() + REMOTE_SESSION_TOTAL_DEADLINE;
    let mut stream = connect(path, deadline).await?;
    write_json(&mut stream, &request, deadline)
        .await
        .map_err(map_frame_error)?;
    let mut budget = response_budget();
    let response = read_response_with_idle_timeout(
        &mut stream,
        deadline,
        REMOTE_SESSION_TOTAL_DEADLINE,
        &mut budget,
        false,
    )
    .await?;
    validate_envelope(&response, request_id)?;
    if response.sequence != 0 || response.snapshot_id.is_some() {
        return Err(ProtocolError::UnexpectedBody.into());
    }
    match response.body {
        ResponseBody::RemoteSession(result) => {
            result
                .validate_for(&command)
                .map_err(|_| ProtocolError::UnexpectedBody)?;
            expect_eof(&mut stream, deadline).await?;
            Ok(result)
        }
        ResponseBody::Error(error) => {
            expect_eof(&mut stream, deadline).await?;
            Err(ClientError::Daemon(error))
        }
        _ => Err(ProtocolError::UnexpectedBody.into()),
    }
}

pub async fn request_terminal(
    path: &Path,
    request: RequestEnvelope,
) -> Result<TerminalResult, ClientError> {
    let RequestBody::Terminal(command) = &request.body else {
        return Err(ProtocolError::RequestBody.into());
    };
    if matches!(command, TerminalCommand::Stream { .. }) {
        return Err(ProtocolError::RequestBody.into());
    }
    command.validate().map_err(|_| ProtocolError::RequestBody)?;
    let command = command.clone();
    let request_id = request.request_id;
    let deadline = Instant::now() + SNAPSHOT_TOTAL_DEADLINE;
    let mut stream = connect(path, deadline).await?;
    write_json(&mut stream, &request, deadline)
        .await
        .map_err(map_frame_error)?;
    let mut budget = response_budget();
    let response = read_response(&mut stream, deadline, &mut budget, false).await?;
    validate_envelope(&response, request_id)?;
    if response.sequence != 0 || response.snapshot_id.is_some() {
        return Err(ProtocolError::UnexpectedBody.into());
    }
    match response.body {
        ResponseBody::Terminal(result) => {
            result
                .validate_for(&command)
                .map_err(|_| ProtocolError::UnexpectedBody)?;
            expect_eof(&mut stream, deadline).await?;
            Ok(result)
        }
        ResponseBody::Error(error) => {
            expect_eof(&mut stream, deadline).await?;
            Err(ClientError::Daemon(error))
        }
        _ => Err(ProtocolError::UnexpectedBody.into()),
    }
}

pub async fn request_terminal_stream<F>(
    path: &Path,
    request: RequestEnvelope,
    mut on_event: F,
) -> Result<(), ClientError>
where
    F: FnMut(TerminalStreamEvent) -> Result<(), ClientError>,
{
    let RequestBody::Terminal(command @ TerminalCommand::Stream { .. }) = &request.body else {
        return Err(ProtocolError::RequestBody.into());
    };
    command.validate().map_err(|_| ProtocolError::RequestBody)?;
    let TerminalCommand::Stream {
        session_id,
        max_bytes,
    } = *command
    else {
        unreachable!("terminal stream command was matched");
    };
    let request_id = request.request_id;
    let mut stream = connect(&path, Instant::now() + HEALTH_TOTAL_DEADLINE).await?;
    write_json(
        &mut stream,
        &request,
        Instant::now() + HEALTH_TOTAL_DEADLINE,
    )
    .await
    .map_err(map_frame_error)?;

    let mut expected_sequence = 0_u32;
    let mut started = false;
    loop {
        let mut budget = response_budget();
        let response = read_response(
            &mut stream,
            Instant::now() + SNAPSHOT_TOTAL_DEADLINE,
            &mut budget,
            started,
        )
        .await?;
        validate_envelope(&response, request_id)?;
        if response.sequence != expected_sequence || response.snapshot_id.is_some() {
            return Err(ProtocolError::Sequence.into());
        }
        expected_sequence = next_sequence(expected_sequence)?;

        match response.body {
            ResponseBody::TerminalStreamStart(TerminalStreamStart {
                session_id: actual,
                max_bytes: actual_max,
                status,
            }) if !started
                && actual == session_id
                && actual_max == max_bytes
                && status.validate().is_ok() =>
            {
                started = true;
                on_event(TerminalStreamEvent::Started {
                    session_id,
                    max_bytes,
                    status,
                })?;
            }
            ResponseBody::TerminalStreamData(TerminalStreamData {
                session_id: actual,
                data,
            }) if started
                && actual == session_id
                && data
                    .decode()
                    .is_ok_and(|bytes| bytes.len() <= max_bytes as usize) =>
            {
                on_event(TerminalStreamEvent::Data { session_id, data })?;
            }
            ResponseBody::TerminalStreamStatus(TerminalStreamStatus {
                session_id: actual,
                status,
            }) if started && actual == session_id && status.validate().is_ok() => {
                on_event(TerminalStreamEvent::Status { session_id, status })?;
            }
            ResponseBody::TerminalStreamEnd(TerminalStreamEnd {
                session_id: actual,
                status,
            }) if started
                && actual == session_id
                && status.validate().is_ok()
                && !matches!(status.state, TerminalState::Running) =>
            {
                on_event(TerminalStreamEvent::Ended { session_id, status })?;
                expect_eof(&mut stream, Instant::now() + HEALTH_TOTAL_DEADLINE).await?;
                return Ok(());
            }
            ResponseBody::Error(error) if !started => {
                expect_eof(&mut stream, Instant::now() + HEALTH_TOTAL_DEADLINE).await?;
                return Err(ClientError::Daemon(error));
            }
            _ => return Err(ProtocolError::UnexpectedBody.into()),
        }
    }
}

pub async fn request_transfer(
    path: &Path,
    request: RequestEnvelope,
) -> Result<TransferOutput, ClientError> {
    let RequestBody::Transfer(command) = &request.body else {
        return Err(ProtocolError::RequestBody.into());
    };
    command.validate().map_err(|_| ProtocolError::RequestBody)?;
    let command = command.clone();
    let request_id = request.request_id;
    let deadline = Instant::now() + SNAPSHOT_TOTAL_DEADLINE;
    let mut stream = connect(path, deadline).await?;
    write_json(&mut stream, &request, deadline)
        .await
        .map_err(map_frame_error)?;
    let mut budget = response_budget();
    let response = read_response(&mut stream, deadline, &mut budget, false).await?;
    validate_envelope(&response, request_id)?;
    if response.sequence != 0 {
        return Err(ProtocolError::Sequence.into());
    }
    match response.body {
        ResponseBody::Transfer(output) => {
            if response.snapshot_id.is_some() || output.validate_for(&command).is_err() {
                return Err(ProtocolError::UnexpectedBody.into());
            }
            expect_eof(&mut stream, deadline).await?;
            Ok(*output)
        }
        ResponseBody::TransferPageStart(start) => {
            let TransferCommand::List { query } = &command else {
                return Err(ProtocolError::UnexpectedBody.into());
            };
            let snapshot_id = response
                .snapshot_id
                .filter(|snapshot_id| !snapshot_id.is_nil())
                .ok_or(ProtocolError::SnapshotIdentity)?;
            validate_transfer_start(&start, query)?;
            read_transfer_page(
                &mut stream,
                deadline,
                &mut budget,
                request_id,
                snapshot_id,
                *start,
                command,
            )
            .await
        }
        ResponseBody::Error(error) => {
            if response.snapshot_id.is_some() {
                return Err(ProtocolError::SnapshotIdentity.into());
            }
            expect_eof(&mut stream, deadline).await?;
            Err(ClientError::Daemon(error))
        }
        _ => Err(ProtocolError::UnexpectedBody.into()),
    }
}

pub async fn request_transfer_local_handle(
    path: &Path,
    request: RequestEnvelope,
) -> Result<TransferLocalHandleGrant, ClientError> {
    let RequestBody::TransferLocalHandle(bind) = &request.body else {
        return Err(ProtocolError::RequestBody.into());
    };
    if !bind.validate() {
        return Err(ProtocolError::RequestBody.into());
    }
    let request_id = request.request_id;
    let deadline = Instant::now() + SNAPSHOT_TOTAL_DEADLINE;
    let mut stream = connect(path, deadline).await?;
    write_json(&mut stream, &request, deadline)
        .await
        .map_err(map_frame_error)?;
    let mut budget = response_budget();
    let response = read_response(&mut stream, deadline, &mut budget, false).await?;
    validate_envelope(&response, request_id)?;
    if response.sequence != 0 || response.snapshot_id.is_some() {
        return Err(ProtocolError::Sequence.into());
    }
    match response.body {
        ResponseBody::TransferLocalHandle(grant) if grant.validate().is_ok() => {
            expect_eof(&mut stream, deadline).await?;
            Ok(grant)
        }
        ResponseBody::Error(error) => {
            expect_eof(&mut stream, deadline).await?;
            Err(ClientError::Daemon(error))
        }
        _ => Err(ProtocolError::UnexpectedBody.into()),
    }
}

#[allow(clippy::too_many_arguments)]
async fn read_transfer_page(
    stream: &mut UnixStream,
    deadline: Instant,
    budget: &mut WireBudget,
    request_id: Uuid,
    snapshot_id: Uuid,
    start: TransferPageStart,
    command: TransferCommand,
) -> Result<TransferOutput, ClientError> {
    let mut expected_sequence = 1_u32;
    let mut data_frames = 0_u32;
    let mut tasks: Vec<TransferTask> = Vec::new();
    loop {
        let response = read_response(stream, deadline, budget, true).await?;
        validate_stream_envelope(&response, request_id, expected_sequence, snapshot_id)?;
        match response.body {
            ResponseBody::TransferTaskChunk(chunk) => {
                validate_chunk_len(chunk.records.len())?;
                if chunk
                    .records
                    .iter()
                    .any(|task| task.validate_public().is_err() || !start.query.matches(task))
                {
                    return Err(ProtocolError::InvalidChunk.into());
                }
                data_frames = data_frames
                    .checked_add(1)
                    .ok_or(ProtocolError::InvalidChunk)?;
                let count = tasks
                    .len()
                    .checked_add(chunk.records.len())
                    .ok_or(ProtocolError::InvalidChunk)?;
                if data_frames > start.data_frame_count
                    || count > start.total_tasks as usize
                    || count > usize::from(MAX_TRANSFER_PAGE_TASKS)
                {
                    return Err(ProtocolError::InvalidChunk.into());
                }
                tasks.extend(chunk.records);
                expected_sequence = next_sequence(expected_sequence)?;
            }
            ResponseBody::TransferPageEnd(end) => {
                validate_transfer_end(&start, &end, data_frames, tasks.len())?;
                let page = TransferPage {
                    query: start.query,
                    tasks,
                    has_more: start.has_more,
                    next_offset: start.next_offset,
                };
                page.validate().map_err(|_| ProtocolError::InvalidEnd)?;
                let output = TransferOutput::Page { page };
                output
                    .validate_for(&command)
                    .map_err(|_| ProtocolError::InvalidEnd)?;
                expect_eof(stream, deadline).await?;
                return Ok(output);
            }
            ResponseBody::Error(error) => {
                expect_eof(stream, deadline).await?;
                return Err(ClientError::Daemon(error));
            }
            _ => return Err(ProtocolError::UnexpectedBody.into()),
        }
    }
}

pub async fn request_notes(
    path: &Path,
    request: RequestEnvelope,
) -> Result<NotesOutput, ClientError> {
    let RequestBody::Notes(command) = &request.body else {
        return Err(ProtocolError::RequestBody.into());
    };
    command.validate().map_err(|_| ProtocolError::RequestBody)?;
    let command = command.clone();
    let request_id = request.request_id;
    let deadline = Instant::now() + SNAPSHOT_TOTAL_DEADLINE;
    let mut stream = connect(path, deadline).await?;
    write_json(&mut stream, &request, deadline)
        .await
        .map_err(map_frame_error)?;

    let mut budget = notes_response_budget(&command);
    let response = read_response(&mut stream, deadline, &mut budget, false).await?;
    validate_envelope(&response, request_id)?;
    if response.sequence != 0 {
        return Err(ProtocolError::Sequence.into());
    }

    match response.body {
        ResponseBody::NotesMutation(result) => {
            if response.snapshot_id.is_some() || !mutation_matches(&command, &result) {
                return Err(ProtocolError::UnexpectedBody.into());
            }
            expect_eof(&mut stream, deadline).await?;
            Ok(NotesOutput::Mutation(result))
        }
        ResponseBody::NotesPageStart(start) => {
            let snapshot_id = response
                .snapshot_id
                .filter(|snapshot_id| !snapshot_id.is_nil())
                .ok_or(ProtocolError::SnapshotIdentity)?;
            let requested_query = match &command {
                NotesCommand::List { query } => query,
                _ => return Err(ProtocolError::UnexpectedBody.into()),
            };
            validate_notes_page_start(&start, requested_query)?;
            read_notes_page(
                &mut stream,
                deadline,
                &mut budget,
                request_id,
                snapshot_id,
                *start,
            )
            .await
        }
        ResponseBody::NotesContentStart(start) => {
            let snapshot_id = response
                .snapshot_id
                .filter(|snapshot_id| !snapshot_id.is_nil())
                .ok_or(ProtocolError::SnapshotIdentity)?;
            validate_notes_content_start(&start, &command)?;
            read_notes_content(
                &mut stream,
                deadline,
                &mut budget,
                request_id,
                snapshot_id,
                *start,
                &command,
            )
            .await
        }
        ResponseBody::Error(error) => {
            if response.snapshot_id.is_some() {
                return Err(ProtocolError::SnapshotIdentity.into());
            }
            expect_eof(&mut stream, deadline).await?;
            Err(ClientError::Daemon(error))
        }
        _ => Err(ProtocolError::UnexpectedBody.into()),
    }
}

#[allow(clippy::too_many_arguments)]
async fn read_notes_page(
    stream: &mut UnixStream,
    deadline: Instant,
    budget: &mut WireBudget,
    request_id: Uuid,
    snapshot_id: Uuid,
    start: NotesPageStart,
) -> Result<NotesOutput, ClientError> {
    let mut expected_sequence = 1_u32;
    let mut data_frames = 0_u32;
    let mut notes = Vec::with_capacity(start.total_notes as usize);
    loop {
        let response = read_response(stream, deadline, budget, true).await?;
        validate_stream_envelope(&response, request_id, expected_sequence, snapshot_id)?;
        match response.body {
            ResponseBody::NoteSummaryChunk(chunk) => {
                validate_chunk_len(chunk.records.len())?;
                if chunk.records.iter().any(|note| note.validate().is_err()) {
                    return Err(ProtocolError::InvalidChunk.into());
                }
                data_frames = data_frames
                    .checked_add(1)
                    .ok_or(ProtocolError::SequenceOverflow)?;
                let next_len = notes
                    .len()
                    .checked_add(chunk.records.len())
                    .ok_or(ProtocolError::InvalidChunk)?;
                if data_frames > start.data_frame_count || next_len > start.total_notes as usize {
                    return Err(ProtocolError::InvalidChunk.into());
                }
                notes.extend(chunk.records);
                expected_sequence = next_sequence(expected_sequence)?;
            }
            ResponseBody::NotesPageEnd(end) => {
                validate_notes_page_end(&start, &end, data_frames, notes.len())?;
                let page = NotePage {
                    query: start.query,
                    notes,
                    has_more: start.has_more,
                    next_offset: start.next_offset,
                };
                page.validate().map_err(|_| ProtocolError::InvalidEnd)?;
                expect_eof(stream, deadline).await?;
                return Ok(NotesOutput::Page(page));
            }
            ResponseBody::Error(error) => {
                expect_eof(stream, deadline).await?;
                return Err(ClientError::Daemon(error));
            }
            _ => return Err(ProtocolError::UnexpectedBody.into()),
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn read_notes_content(
    stream: &mut UnixStream,
    deadline: Instant,
    budget: &mut WireBudget,
    request_id: Uuid,
    snapshot_id: Uuid,
    start: NotesContentStart,
    command: &NotesCommand,
) -> Result<NotesOutput, ClientError> {
    let mut expected_sequence = 1_u32;
    let mut data_frames = 0_u32;
    let mut bytes = Vec::with_capacity(start.total_bytes as usize);
    loop {
        let response = read_response(stream, deadline, budget, true).await?;
        validate_stream_envelope(&response, request_id, expected_sequence, snapshot_id)?;
        match response.body {
            ResponseBody::NotesContentChunk(chunk) => {
                if chunk.offset as usize != bytes.len()
                    || chunk.raw_bytes == 0
                    || chunk.raw_bytes as usize > NOTE_CONTENT_CHUNK_BYTES
                    || chunk.data_base64.len() > MAX_NOTE_CONTENT_BASE64_BYTES
                {
                    return Err(ProtocolError::InvalidChunk.into());
                }
                let decoded = STANDARD
                    .decode(chunk.data_base64)
                    .map_err(|_| ProtocolError::InvalidChunk)?;
                if decoded.len() != chunk.raw_bytes as usize {
                    return Err(ProtocolError::InvalidChunk.into());
                }
                data_frames = data_frames
                    .checked_add(1)
                    .ok_or(ProtocolError::SequenceOverflow)?;
                let next_len = bytes
                    .len()
                    .checked_add(decoded.len())
                    .ok_or(ProtocolError::InvalidChunk)?;
                if data_frames > start.data_frame_count || next_len > start.total_bytes as usize {
                    return Err(ProtocolError::InvalidChunk.into());
                }
                bytes.extend(decoded);
                expected_sequence = next_sequence(expected_sequence)?;
            }
            ResponseBody::NotesContentEnd(end) => {
                validate_notes_content_end(&start, &end, data_frames, bytes.len())?;
                let actual_sha256 = format!("{:x}", Sha256::digest(&bytes));
                if actual_sha256 != start.content_sha256 {
                    return Err(ProtocolError::InvalidEnd.into());
                }
                let content = String::from_utf8(bytes).map_err(|_| ProtocolError::InvalidEnd)?;
                let output = match (&start.kind, command) {
                    (NotesContentKind::Document, NotesCommand::Get { id }) => {
                        let summary = start.note.ok_or(ProtocolError::InvalidEnd)?;
                        if summary.id != *id || summary.body_sha256 != start.content_sha256 {
                            return Err(ProtocolError::InvalidEnd.into());
                        }
                        let document = NoteDocument {
                            summary,
                            body_markdown: content,
                        };
                        document.validate().map_err(|_| ProtocolError::InvalidEnd)?;
                        NotesOutput::Document(document)
                    }
                    (
                        NotesContentKind::Export { format },
                        NotesCommand::Export {
                            format: requested, ..
                        },
                    ) if format == requested => {
                        if start.note.is_some() {
                            return Err(ProtocolError::InvalidEnd.into());
                        }
                        let export = NoteExport {
                            format: *format,
                            content_bytes: start.total_bytes,
                            content_sha256: start.content_sha256,
                            content,
                        };
                        export.validate().map_err(|_| ProtocolError::InvalidEnd)?;
                        NotesOutput::Export(export)
                    }
                    _ => return Err(ProtocolError::UnexpectedBody.into()),
                };
                expect_eof(stream, deadline).await?;
                return Ok(output);
            }
            ResponseBody::Error(error) => {
                expect_eof(stream, deadline).await?;
                return Err(ClientError::Daemon(error));
            }
            _ => return Err(ProtocolError::UnexpectedBody.into()),
        }
    }
}

pub async fn request_network_snapshot(
    path: &Path,
    request: RequestEnvelope,
) -> Result<NetworkSnapshot, ClientError> {
    if !matches!(request.body, RequestBody::NetworkSnapshot(_)) {
        return Err(ProtocolError::RequestBody.into());
    }
    let request_id = request.request_id;
    let deadline = Instant::now() + SNAPSHOT_TOTAL_DEADLINE;
    let mut stream = connect(path, deadline).await?;
    write_json(&mut stream, &request, deadline)
        .await
        .map_err(map_frame_error)?;

    let mut budget = response_budget();
    let response = read_response(&mut stream, deadline, &mut budget, false).await?;
    validate_envelope(&response, request_id)?;
    if response.sequence != 0 {
        return Err(ProtocolError::Sequence.into());
    }
    let snapshot_id = response.snapshot_id.filter(|id| !id.is_nil());
    let start = match response.body {
        ResponseBody::NetworkSnapshotStart(start) => {
            if snapshot_id.is_none() {
                return Err(ProtocolError::SnapshotIdentity.into());
            }
            validate_network_start(&start)?;
            start
        }
        ResponseBody::Error(error) => {
            if response.snapshot_id.is_some() {
                return Err(ProtocolError::SnapshotIdentity.into());
            }
            expect_eof(&mut stream, deadline).await?;
            return Err(ClientError::Daemon(error));
        }
        _ => return Err(ProtocolError::UnexpectedBody.into()),
    };
    let snapshot_id = snapshot_id.expect("validated snapshot identity");
    let mut expected_sequence = 1_u32;
    let mut data_frames = 0_u32;
    let mut total_records = 0_usize;
    let mut interfaces = Vec::with_capacity(start.total_interfaces as usize);
    let mut applications = Vec::with_capacity(start.total_applications as usize);
    let mut application_phase = false;

    loop {
        let response = read_response(&mut stream, deadline, &mut budget, true).await?;
        validate_stream_envelope(&response, request_id, expected_sequence, snapshot_id)?;
        match response.body {
            ResponseBody::NetworkInterfaceChunk(chunk) => {
                if application_phase {
                    return Err(ProtocolError::InvalidChunk.into());
                }
                validate_chunk_len(chunk.records.len())?;
                checked_stream_progress(
                    &start,
                    &mut data_frames,
                    &mut total_records,
                    chunk.records.len(),
                )?;
                if interfaces.len() + chunk.records.len() > start.total_interfaces as usize {
                    return Err(ProtocolError::InvalidChunk.into());
                }
                interfaces.extend(chunk.records);
                expected_sequence = next_sequence(expected_sequence)?;
            }
            ResponseBody::NetworkApplicationChunk(chunk) => {
                application_phase = true;
                validate_chunk_len(chunk.records.len())?;
                checked_stream_progress(
                    &start,
                    &mut data_frames,
                    &mut total_records,
                    chunk.records.len(),
                )?;
                if applications.len() + chunk.records.len() > start.total_applications as usize {
                    return Err(ProtocolError::InvalidChunk.into());
                }
                applications.extend(chunk.records);
                expected_sequence = next_sequence(expected_sequence)?;
            }
            ResponseBody::NetworkSnapshotEnd(end) => {
                validate_network_end(
                    &start,
                    &end,
                    data_frames,
                    total_records,
                    interfaces.len(),
                    applications.len(),
                )?;
                let snapshot = NetworkSnapshot {
                    schema_version: start.schema_version,
                    snapshot_id,
                    captured_at_unix_ms: start.captured_at_unix_ms,
                    observed_boottime_ms: start.observed_boottime_ms,
                    sample_interval_ms: start.sample_interval_ms,
                    last_success_at_unix_ms: start.last_success_at_unix_ms,
                    freshness: start.freshness,
                    retryable: start.retryable,
                    system_traffic: start.system_traffic,
                    per_application: start.per_application,
                    coverage: start.coverage,
                    totals: start.totals,
                    aggregate_rate: start.aggregate_rate,
                    interfaces,
                    applications,
                };
                snapshot.validate().map_err(|_| ProtocolError::InvalidEnd)?;
                expect_eof(&mut stream, deadline).await?;
                return Ok(snapshot);
            }
            ResponseBody::Error(error) => {
                expect_eof(&mut stream, deadline).await?;
                return Err(ClientError::Daemon(error));
            }
            _ => return Err(ProtocolError::UnexpectedBody.into()),
        }
    }
}

pub async fn request_usage_summary(
    path: &Path,
    request: RequestEnvelope,
) -> Result<UsageSummary, ClientError> {
    let RequestBody::UsageSummary(usage_request) = &request.body else {
        return Err(ProtocolError::RequestBody.into());
    };
    usage_request
        .query
        .validate()
        .map_err(|_| ProtocolError::RequestBody)?;
    let requested_query = usage_request.query.clone();
    let request_id = request.request_id;
    let deadline = Instant::now() + SNAPSHOT_TOTAL_DEADLINE;
    let mut stream = connect(path, deadline).await?;
    write_json(&mut stream, &request, deadline)
        .await
        .map_err(map_frame_error)?;

    let mut budget = response_budget();
    let response = read_response(&mut stream, deadline, &mut budget, false).await?;
    validate_envelope(&response, request_id)?;
    if response.sequence != 0 {
        return Err(ProtocolError::Sequence.into());
    }
    let snapshot_id = response.snapshot_id.filter(|id| !id.is_nil());
    let start = match response.body {
        ResponseBody::UsageSummaryStart(start) => {
            if snapshot_id.is_none() {
                return Err(ProtocolError::SnapshotIdentity.into());
            }
            validate_usage_start(&start, &requested_query)?;
            start
        }
        ResponseBody::Error(error) => {
            if response.snapshot_id.is_some() {
                return Err(ProtocolError::SnapshotIdentity.into());
            }
            expect_eof(&mut stream, deadline).await?;
            return Err(ClientError::Daemon(error));
        }
        _ => return Err(ProtocolError::UnexpectedBody.into()),
    };
    let snapshot_id = snapshot_id.expect("validated snapshot identity");
    let mut expected_sequence = 1_u32;
    let mut data_frames = 0_u32;
    let mut total_records = 0_usize;
    let mut applications = Vec::with_capacity(start.total_applications as usize);

    loop {
        let response = read_response(&mut stream, deadline, &mut budget, true).await?;
        validate_stream_envelope(&response, request_id, expected_sequence, snapshot_id)?;
        match response.body {
            ResponseBody::UsageApplicationChunk(chunk) => {
                validate_chunk_len(chunk.records.len())?;
                data_frames = data_frames
                    .checked_add(1)
                    .ok_or(ProtocolError::SequenceOverflow)?;
                total_records = total_records
                    .checked_add(chunk.records.len())
                    .ok_or(ProtocolError::InvalidChunk)?;
                if data_frames > start.data_frame_count
                    || total_records > start.total_records as usize
                    || applications.len() + chunk.records.len() > start.total_applications as usize
                {
                    return Err(ProtocolError::InvalidChunk.into());
                }
                applications.extend(chunk.records);
                expected_sequence = next_sequence(expected_sequence)?;
            }
            ResponseBody::UsageSummaryEnd(end) => {
                validate_usage_end(&start, &end, data_frames, total_records, applications.len())?;
                let summary = UsageSummary {
                    schema_version: start.schema_version,
                    snapshot_id,
                    captured_at_unix_ms: start.captured_at_unix_ms,
                    query: start.query,
                    status: start.status,
                    reason: start.reason,
                    retryable: start.retryable,
                    coverage: start.coverage,
                    applications,
                };
                summary.validate().map_err(|_| ProtocolError::InvalidEnd)?;
                expect_eof(&mut stream, deadline).await?;
                return Ok(summary);
            }
            ResponseBody::Error(error) => {
                expect_eof(&mut stream, deadline).await?;
                return Err(ClientError::Daemon(error));
            }
            _ => return Err(ProtocolError::UnexpectedBody.into()),
        }
    }
}

fn validate_stream_envelope(
    response: &ResponseEnvelope,
    request_id: Uuid,
    expected_sequence: u32,
    snapshot_id: Uuid,
) -> Result<(), ProtocolError> {
    validate_envelope(response, request_id)?;
    if response.sequence != expected_sequence {
        return Err(ProtocolError::Sequence);
    }
    if response.snapshot_id != Some(snapshot_id) {
        return Err(ProtocolError::SnapshotIdentity);
    }
    Ok(())
}

fn mutation_matches(command: &NotesCommand, result: &NoteMutationResult) -> bool {
    match (command, result) {
        (
            NotesCommand::WriteInline { intent, .. } | NotesCommand::UploadCommit { intent, .. },
            NoteMutationResult::Stored(note),
        ) => {
            note.validate().is_ok()
                && match intent {
                    localdesk_domain::NoteWriteIntent::Create => true,
                    localdesk_domain::NoteWriteIntent::Save { id, .. } => note.id == *id,
                }
        }
        (NotesCommand::Delete { id, .. }, NoteMutationResult::Deleted(note))
        | (NotesCommand::Restore { id, .. }, NoteMutationResult::Restored(note)) => {
            note.id == *id && note.validate().is_ok()
        }
        (
            NotesCommand::WriteInline { intent, .. } | NotesCommand::UploadCommit { intent, .. },
            NoteMutationResult::Conflict {
                expected_revision,
                current,
            },
        ) => {
            matches!(
                intent,
                localdesk_domain::NoteWriteIntent::Save {
                    expected_revision: requested,
                    ..
                } if requested == expected_revision
            ) && current.validate().is_ok()
                && matches!(intent, localdesk_domain::NoteWriteIntent::Save { id, .. } if current.id == *id)
        }
        (
            NotesCommand::Delete {
                id,
                expected_revision: requested,
            }
            | NotesCommand::Restore {
                id,
                expected_revision: requested,
            },
            NoteMutationResult::Conflict {
                expected_revision,
                current,
            },
        ) => requested == expected_revision && current.id == *id && current.validate().is_ok(),
        (
            NotesCommand::UploadBegin { .. },
            NoteMutationResult::UploadBegun {
                upload_id,
                max_chunk_raw_bytes,
            },
        ) => !upload_id.is_nil() && *max_chunk_raw_bytes as usize == NOTE_CONTENT_CHUNK_BYTES,
        (
            NotesCommand::UploadAppend {
                upload_id,
                sequence,
                offset,
                data_base64,
            },
            NoteMutationResult::UploadAccepted {
                upload_id: returned_id,
                next_sequence,
                next_offset,
            },
        ) => {
            let Ok(decoded) = STANDARD.decode(data_base64) else {
                return false;
            };
            returned_id == upload_id
                && sequence.checked_add(1) == Some(*next_sequence)
                && offset.checked_add(decoded.len() as u32) == Some(*next_offset)
        }
        (
            NotesCommand::UploadAbort { upload_id },
            NoteMutationResult::UploadAborted {
                upload_id: returned_id,
            },
        ) => returned_id == upload_id,
        _ => false,
    }
}

fn validate_transfer_start(
    start: &TransferPageStart,
    requested_query: &localdesk_transfers::TransferQuery,
) -> Result<(), ProtocolError> {
    let total_tasks = start.total_tasks as usize;
    validate_stream_declaration(total_tasks, start.data_frame_count)?;
    let expected_next = start
        .query
        .offset
        .checked_add(start.total_tasks)
        .ok_or(ProtocolError::InvalidStart)?;
    if start.schema_version != TRANSFER_PUBLIC_SCHEMA_VERSION
        || start.query != *requested_query
        || start.query.validate().is_err()
        || total_tasks > usize::from(start.query.limit)
        || total_tasks > usize::from(MAX_TRANSFER_PAGE_TASKS)
        || (start.has_more && (total_tasks == 0 || start.next_offset != Some(expected_next)))
        || (!start.has_more && start.next_offset.is_some())
    {
        return Err(ProtocolError::InvalidStart);
    }
    Ok(())
}

fn validate_transfer_end(
    start: &TransferPageStart,
    end: &TransferPageEnd,
    data_frames: u32,
    total_tasks: usize,
) -> Result<(), ProtocolError> {
    if end.schema_version != start.schema_version
        || end.query != start.query
        || end.total_tasks != start.total_tasks
        || end.data_frame_count != start.data_frame_count
        || end.has_more != start.has_more
        || end.next_offset != start.next_offset
        || data_frames != start.data_frame_count
        || total_tasks != start.total_tasks as usize
    {
        return Err(ProtocolError::InvalidEnd);
    }
    Ok(())
}

fn validate_notes_page_start(
    start: &NotesPageStart,
    requested_query: &localdesk_domain::NoteQuery,
) -> Result<(), ProtocolError> {
    validate_stream_declaration(start.total_notes as usize, start.data_frame_count)?;
    if start.schema_version != NOTES_SCHEMA_VERSION
        || start.query != *requested_query
        || start.query.validate().is_err()
        || start.total_notes > start.query.limit
        || (start.has_more && start.next_offset.is_none())
        || (!start.has_more && start.next_offset.is_some())
    {
        return Err(ProtocolError::InvalidStart);
    }
    Ok(())
}

fn validate_notes_page_end(
    start: &NotesPageStart,
    end: &NotesPageEnd,
    data_frames: u32,
    total_notes: usize,
) -> Result<(), ProtocolError> {
    if end.schema_version != start.schema_version
        || end.query != start.query
        || end.total_notes != start.total_notes
        || end.data_frame_count != start.data_frame_count
        || end.has_more != start.has_more
        || end.next_offset != start.next_offset
        || data_frames != start.data_frame_count
        || total_notes != start.total_notes as usize
    {
        return Err(ProtocolError::InvalidEnd);
    }
    Ok(())
}

fn validate_notes_content_start(
    start: &NotesContentStart,
    command: &NotesCommand,
) -> Result<(), ProtocolError> {
    let total_bytes = start.total_bytes as usize;
    let expected_frames = if total_bytes == 0 {
        0
    } else {
        total_bytes.div_ceil(NOTE_CONTENT_CHUNK_BYTES)
    };
    let kind_matches = match (&start.kind, command) {
        (NotesContentKind::Document, NotesCommand::Get { .. }) => true,
        (
            NotesContentKind::Export { format: actual },
            NotesCommand::Export {
                format: requested, ..
            },
        ) => actual == requested,
        _ => false,
    };
    let size_valid = match start.kind {
        NotesContentKind::Document => total_bytes <= MAX_NOTE_BODY_BYTES,
        NotesContentKind::Export { .. } => total_bytes <= MAX_NOTE_EXPORT_BYTES,
    };
    let note_valid = match &start.kind {
        NotesContentKind::Document => start.note.as_ref().is_some_and(|note| {
            note.validate().is_ok()
                && note.body_bytes == start.total_bytes
                && note.body_sha256 == start.content_sha256
        }),
        NotesContentKind::Export { .. } => start.note.is_none(),
    };
    if start.schema_version != NOTES_SCHEMA_VERSION
        || !kind_matches
        || !size_valid
        || !note_valid
        || localdesk_domain::validate_sha256(&start.content_sha256).is_err()
        || start.data_frame_count as usize != expected_frames
    {
        return Err(ProtocolError::InvalidStart);
    }
    Ok(())
}

fn validate_notes_content_end(
    start: &NotesContentStart,
    end: &NotesContentEnd,
    data_frames: u32,
    total_bytes: usize,
) -> Result<(), ProtocolError> {
    if end.schema_version != start.schema_version
        || end.kind != start.kind
        || end.total_bytes != start.total_bytes
        || end.content_sha256 != start.content_sha256
        || end.data_frame_count != start.data_frame_count
        || data_frames != start.data_frame_count
        || total_bytes != start.total_bytes as usize
    {
        return Err(ProtocolError::InvalidEnd);
    }
    Ok(())
}

fn validate_chunk_len(record_count: usize) -> Result<(), ProtocolError> {
    if record_count == 0 || record_count > MAX_CHUNK_RECORDS {
        return Err(ProtocolError::InvalidChunk);
    }
    Ok(())
}

fn checked_stream_progress(
    start: &NetworkSnapshotStart,
    data_frames: &mut u32,
    total_records: &mut usize,
    record_count: usize,
) -> Result<(), ProtocolError> {
    *data_frames = data_frames
        .checked_add(1)
        .ok_or(ProtocolError::SequenceOverflow)?;
    *total_records = total_records
        .checked_add(record_count)
        .ok_or(ProtocolError::InvalidChunk)?;
    if *data_frames > start.data_frame_count || *total_records > start.total_records as usize {
        return Err(ProtocolError::InvalidChunk);
    }
    Ok(())
}

fn validate_network_start(start: &NetworkSnapshotStart) -> Result<(), ProtocolError> {
    let interfaces = start.total_interfaces as usize;
    let applications = start.total_applications as usize;
    let total_records = start.total_records as usize;
    let expected_total = interfaces
        .checked_add(applications)
        .ok_or(ProtocolError::InvalidStart)?;
    validate_stream_declaration(total_records, start.data_frame_count)?;
    if start.schema_version != NETWORK_SCHEMA_VERSION
        || interfaces > MAX_NETWORK_INTERFACES
        || applications > MAX_NETWORK_APPLICATIONS
        || total_records != expected_total
    {
        return Err(ProtocolError::InvalidStart);
    }
    Ok(())
}

fn validate_network_end(
    start: &NetworkSnapshotStart,
    end: &NetworkSnapshotEnd,
    data_frames: u32,
    total_records: usize,
    interfaces: usize,
    applications: usize,
) -> Result<(), ProtocolError> {
    if end.schema_version != start.schema_version
        || end.total_interfaces != start.total_interfaces
        || end.total_applications != start.total_applications
        || end.total_records != start.total_records
        || end.data_frame_count != start.data_frame_count
        || data_frames != start.data_frame_count
        || total_records != start.total_records as usize
        || interfaces != start.total_interfaces as usize
        || applications != start.total_applications as usize
    {
        return Err(ProtocolError::InvalidEnd);
    }
    Ok(())
}

fn validate_usage_start(
    start: &UsageSummaryStart,
    requested_query: &UsageSummaryQuery,
) -> Result<(), ProtocolError> {
    let applications = start.total_applications as usize;
    let total_records = start.total_records as usize;
    validate_stream_declaration(total_records, start.data_frame_count)?;
    if start.schema_version != USAGE_SCHEMA_VERSION
        || start.query != *requested_query
        || start.query.validate().is_err()
        || applications > MAX_USAGE_APPLICATIONS
        || total_records != applications
    {
        return Err(ProtocolError::InvalidStart);
    }
    Ok(())
}

fn validate_usage_end(
    start: &UsageSummaryStart,
    end: &UsageSummaryEnd,
    data_frames: u32,
    total_records: usize,
    applications: usize,
) -> Result<(), ProtocolError> {
    if end.schema_version != start.schema_version
        || end.query != start.query
        || end.status != start.status
        || end.reason != start.reason
        || end.retryable != start.retryable
        || end.total_applications != start.total_applications
        || end.total_records != start.total_records
        || end.data_frame_count != start.data_frame_count
        || data_frames != start.data_frame_count
        || total_records != start.total_records as usize
        || applications != start.total_applications as usize
    {
        return Err(ProtocolError::InvalidEnd);
    }
    Ok(())
}

fn validate_stream_declaration(
    total_records: usize,
    data_frame_count: u32,
) -> Result<(), ProtocolError> {
    let data_frames = data_frame_count as usize;
    let response_frames = data_frame_count
        .checked_add(2)
        .ok_or(ProtocolError::InvalidStart)? as usize;
    if data_frames > MAX_RESPONSE_FRAMES.saturating_sub(2)
        || response_frames > MAX_RESPONSE_FRAMES
        || (total_records == 0) != (data_frames == 0)
        || (total_records > 0 && data_frames > total_records)
    {
        return Err(ProtocolError::InvalidStart);
    }
    Ok(())
}

async fn connect(path: &Path, deadline: Instant) -> Result<UnixStream, ClientError> {
    let stream = timeout_at(deadline, UnixStream::connect(path))
        .await
        .map_err(|_| TransportError::Timeout)?
        .map_err(TransportError::Io)?;
    verify_peer_uid(&stream).map_err(TransportError::Peer)?;
    Ok(stream)
}

fn response_budget() -> WireBudget {
    WireBudget::new(MAX_RESPONSE_FRAMES, MAX_RESPONSE_WIRE_BYTES)
}

fn notes_response_budget(command: &NotesCommand) -> WireBudget {
    if matches!(command, NotesCommand::Export { .. }) {
        WireBudget::new(MAX_NOTES_EXPORT_FRAMES, MAX_NOTES_EXPORT_WIRE_BYTES)
    } else {
        response_budget()
    }
}

async fn read_response(
    stream: &mut UnixStream,
    deadline: Instant,
    budget: &mut WireBudget,
    terminal_required: bool,
) -> Result<ResponseEnvelope, ClientError> {
    match read_json(stream, deadline, budget).await {
        Ok(response) => Ok(response),
        Err(FrameError::UnexpectedEof) if terminal_required => {
            Err(ProtocolError::MissingTerminal.into())
        }
        Err(error) => Err(map_frame_error(error)),
    }
}

async fn read_response_with_idle_timeout(
    stream: &mut UnixStream,
    deadline: Instant,
    idle_timeout: Duration,
    budget: &mut WireBudget,
    terminal_required: bool,
) -> Result<ResponseEnvelope, ClientError> {
    match read_json_with_idle_timeout(stream, deadline, idle_timeout, budget).await {
        Ok(response) => Ok(response),
        Err(FrameError::UnexpectedEof) if terminal_required => {
            Err(ProtocolError::MissingTerminal.into())
        }
        Err(error) => Err(map_frame_error(error)),
    }
}

fn validate_envelope(response: &ResponseEnvelope, request_id: Uuid) -> Result<(), ProtocolError> {
    if response.protocol_version != WIRE_PROTOCOL_VERSION {
        return Err(ProtocolError::Version);
    }
    if response.request_id != request_id {
        return Err(ProtocolError::RequestId);
    }
    Ok(())
}

fn validate_start(start: &SnapshotStart) -> Result<(), ProtocolError> {
    let total_applications = start.total_applications as usize;
    let total_records = start.total_records as usize;
    let data_frame_count = start.data_frame_count as usize;
    let response_frame_count = start
        .data_frame_count
        .checked_add(2)
        .ok_or(ProtocolError::InvalidStart)? as usize;
    if start.schema_version != TELEMETRY_SCHEMA_VERSION
        || total_applications > MAX_APPLICATION_RECORDS
        || total_records > MAX_TOTAL_RECORDS
        || total_records != total_applications
        || data_frame_count > MAX_RESPONSE_FRAMES.saturating_sub(2)
        || response_frame_count > MAX_RESPONSE_FRAMES
        || (total_records == 0) != (data_frame_count == 0)
        || (total_records > 0 && data_frame_count > total_records)
    {
        return Err(ProtocolError::InvalidStart);
    }
    Ok(())
}

fn validate_end(
    start: &SnapshotStart,
    end: &SnapshotEnd,
    data_frames: u32,
    total_records: usize,
    applications: usize,
) -> Result<(), ProtocolError> {
    if end.schema_version != start.schema_version
        || end.freshness != start.freshness
        || end.status != start.status
        || end.reason != start.reason
        || end.retryable != start.retryable
        || end.scope != start.scope
        || end.last_success_at_unix_ms != start.last_success_at_unix_ms
        || end.total_applications != start.total_applications
        || end.total_records != start.total_records
        || end.data_frame_count != start.data_frame_count
        || data_frames != start.data_frame_count
        || total_records != start.total_records as usize
        || applications != start.total_applications as usize
    {
        return Err(ProtocolError::InvalidEnd);
    }
    Ok(())
}

async fn expect_eof(stream: &mut UnixStream, deadline: Instant) -> Result<(), ClientError> {
    let mut trailing = [0_u8; 1];
    let read = timeout_at(deadline, stream.read(&mut trailing))
        .await
        .map_err(|_| TransportError::Timeout)?
        .map_err(TransportError::Io)?;
    if read == 0 {
        Ok(())
    } else {
        Err(ProtocolError::TrailingData.into())
    }
}

fn map_frame_error(error: FrameError) -> ClientError {
    match error {
        FrameError::Io(_)
        | FrameError::IdleTimeout
        | FrameError::DeadlineExceeded
        | FrameError::UnexpectedEof => ClientError::Transport(TransportError::Frame(error)),
        FrameError::Empty
        | FrameError::Oversize
        | FrameError::FrameLimitExceeded
        | FrameError::WireBytesExceeded
        | FrameError::BudgetOverflow
        | FrameError::InvalidJson(_) => ClientError::Protocol(ProtocolError::InvalidFrame),
    }
}

fn next_sequence(sequence: u32) -> Result<u32, ProtocolError> {
    sequence
        .checked_add(1)
        .ok_or(ProtocolError::SequenceOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_overflow_is_rejected() {
        assert_eq!(
            next_sequence(u32::MAX),
            Err(ProtocolError::SequenceOverflow)
        );
    }
}
