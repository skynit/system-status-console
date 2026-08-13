use base64::{Engine as _, engine::general_purpose::STANDARD};
use localdesk_domain::{
    APPD_HEALTH_CAPABILITY, NETWORK_PER_APP_CAPABILITY, NETWORK_SYSTEM_CAPABILITY,
    NOTE_CONTENT_CHUNK_BYTES, NOTES_CAPABILITY, NetworkSnapshot, NoteDocument, NoteDraftMeta,
    NoteExport, NoteExportFormat, NoteMutationResult, NotePage, NoteQuery, NoteWriteIntent,
    NotesCommand, NotesOutput, REMOTE_FTP_CAPABILITY, REMOTE_SFTP_CAPABILITY,
    REMOTE_SMB_CAPABILITY, REMOTE_SSH_CAPABILITY, SpeedTestBasicEnd, SpeedTestCancelResult,
    SpeedTestDeepCommand, SpeedTestDeepOutput, SpeedTestStageData, TELEMETRY_SNAPSHOT_CAPABILITY,
    TRANSFERS_CAPABILITY, TelemetrySnapshot, USAGE_FOREGROUND_CAPABILITY, UsageSummary,
    UsageSummaryQuery,
};
use localdesk_ipc::{
    ClientError, HealthReport, MAX_FRAME_PAYLOAD_BYTES, RequestEnvelope, SystemInfoReport,
    TerminalStreamEvent, TransferLocalHandleBind, request_health, request_network_snapshot,
    request_notes, request_remote_capabilities, request_remote_profile, request_remote_session,
    request_secret, request_speedtest_basic, request_speedtest_cancel, request_speedtest_deep,
    request_system_info, request_telemetry_snapshot, request_terminal, request_terminal_stream,
    request_transfer, request_transfer_local_handle, request_usage_summary,
};
use localdesk_remote_core::{
    RemoteAdapterCatalog, RemoteProfileCommand, RemoteProfileResult, RemoteSessionCommand,
    RemoteSessionResult, SecretCommand, SecretCommandResult, TerminalCommand, TerminalResult,
    TerminalSessionId,
};
use localdesk_transfers::{
    ConflictPolicy, TransferCommand, TransferDraft, TransferId, TransferLocalHandleGrant,
    TransferLocalHandlePurpose, TransferMutationResult, TransferOutput, TransferPage,
    TransferQuery, TransferTask,
};
use nix::unistd::Uid;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};
use tauri::ipc::Channel;
use tauri_plugin_dialog::DialogExt;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BridgeErrorKind {
    Transport,
    Protocol,
    Daemon,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct BridgeError {
    pub kind: BridgeErrorKind,
    pub code: String,
    pub reason: String,
    pub retryable: bool,
}

impl BridgeError {
    fn transport(code: impl Into<String>, retryable: bool) -> Self {
        let code = code.into();
        Self {
            kind: BridgeErrorKind::Transport,
            reason: code.clone(),
            code,
            retryable,
        }
    }
}

impl From<ClientError> for BridgeError {
    fn from(error: ClientError) -> Self {
        match error {
            ClientError::Transport(error) => Self::transport(error.reason_code(), true),
            ClientError::Protocol(error) => {
                let code = error.reason_code().to_owned();
                Self {
                    kind: BridgeErrorKind::Protocol,
                    reason: code.clone(),
                    code,
                    retryable: false,
                }
            }
            ClientError::Daemon(error) => Self {
                kind: BridgeErrorKind::Daemon,
                code: error.code,
                reason: error.reason,
                retryable: error.retryable,
            },
        }
    }
}

#[tauri::command]
pub async fn appd_health() -> Result<HealthReport, BridgeError> {
    let path = runtime_socket_path().map_err(|error| error.into_bridge_error())?;
    request_health(&path, health_request())
        .await
        .map_err(Into::into)
}

fn health_request() -> RequestEnvelope {
    RequestEnvelope::health(
        env!("CARGO_PKG_VERSION"),
        vec![
            APPD_HEALTH_CAPABILITY.to_owned(),
            TELEMETRY_SNAPSHOT_CAPABILITY.to_owned(),
            NETWORK_SYSTEM_CAPABILITY.to_owned(),
            NETWORK_PER_APP_CAPABILITY.to_owned(),
            USAGE_FOREGROUND_CAPABILITY.to_owned(),
            REMOTE_SSH_CAPABILITY.to_owned(),
            REMOTE_SFTP_CAPABILITY.to_owned(),
            REMOTE_FTP_CAPABILITY.to_owned(),
            REMOTE_SMB_CAPABILITY.to_owned(),
            TRANSFERS_CAPABILITY.to_owned(),
            NOTES_CAPABILITY.to_owned(),
        ],
    )
}

#[tauri::command]
pub async fn telemetry_snapshot() -> Result<TelemetrySnapshot, BridgeError> {
    let path = runtime_socket_path().map_err(|error| error.into_bridge_error())?;
    request_telemetry_snapshot(&path, RequestEnvelope::telemetry_snapshot())
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn network_snapshot() -> Result<NetworkSnapshot, BridgeError> {
    let path = runtime_socket_path().map_err(|error| error.into_bridge_error())?;
    request_network_snapshot(&path, RequestEnvelope::network_snapshot())
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn usage_summary(query: UsageSummaryQuery) -> Result<UsageSummary, BridgeError> {
    let path = runtime_socket_path().map_err(|error| error.into_bridge_error())?;
    request_usage_summary(&path, RequestEnvelope::usage_summary(query))
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn speedtest_basic(
    on_stage: Channel<SpeedTestStageData>,
) -> Result<SpeedTestBasicEnd, BridgeError> {
    let path = runtime_socket_path().map_err(|error| error.into_bridge_error())?;
    request_speedtest_basic(
        &path,
        RequestEnvelope::speedtest_basic(),
        move |stage| {
            on_stage.send(stage).map_err(|_| {
                ClientError::Protocol(localdesk_ipc::ProtocolError::UnexpectedBody)
            })
        },
    )
    .await
    .map_err(Into::into)
}

#[tauri::command]
pub async fn speedtest_cancel() -> Result<SpeedTestCancelResult, BridgeError> {
    let path = runtime_socket_path().map_err(|error| error.into_bridge_error())?;
    request_speedtest_cancel(&path, RequestEnvelope::speedtest_cancel())
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn speedtest_deep(
    command: SpeedTestDeepCommand,
) -> Result<SpeedTestDeepOutput, BridgeError> {
    let path = runtime_socket_path().map_err(|error| error.into_bridge_error())?;
    request_speedtest_deep(&path, RequestEnvelope::speedtest_deep(command))
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn system_info() -> Result<SystemInfoReport, BridgeError> {
    let path = runtime_socket_path().map_err(|error| error.into_bridge_error())?;
    request_system_info(&path, RequestEnvelope::system_info())
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn remote_capabilities() -> Result<RemoteAdapterCatalog, BridgeError> {
    let path = runtime_socket_path().map_err(|error| error.into_bridge_error())?;
    request_remote_capabilities(&path, RequestEnvelope::remote_capabilities())
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn remote_profile(
    command: RemoteProfileCommand,
) -> Result<RemoteProfileResult, BridgeError> {
    let path = runtime_socket_path().map_err(|error| error.into_bridge_error())?;
    request_remote_profile(&path, RequestEnvelope::remote_profile(command))
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn secret(command: SecretCommand) -> Result<SecretCommandResult, BridgeError> {
    let path = runtime_socket_path().map_err(|error| error.into_bridge_error())?;
    request_secret(&path, RequestEnvelope::secret(command))
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn remote_session(
    command: RemoteSessionCommand,
) -> Result<RemoteSessionResult, BridgeError> {
    let path = runtime_socket_path().map_err(|error| error.into_bridge_error())?;
    request_remote_session(&path, RequestEnvelope::remote_session(command))
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn remote_terminal(command: TerminalCommand) -> Result<TerminalResult, BridgeError> {
    let path = runtime_socket_path().map_err(|error| error.into_bridge_error())?;
    request_terminal(&path, RequestEnvelope::terminal(command))
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn remote_terminal_stream(
    session_id: TerminalSessionId,
    max_bytes: u32,
    on_event: Channel<TerminalStreamEvent>,
) -> Result<(), BridgeError> {
    let path = runtime_socket_path().map_err(|error| error.into_bridge_error())?;
    request_terminal_stream(
        &path,
        RequestEnvelope::terminal(TerminalCommand::Stream {
            session_id,
            max_bytes,
        }),
        move |event| {
            on_event
                .send(event)
                .map_err(|_| ClientError::Protocol(localdesk_ipc::ProtocolError::UnexpectedBody))
        },
    )
    .await
    .map_err(Into::into)
}

#[tauri::command]
pub async fn transfer_enqueue(draft: TransferDraft) -> Result<TransferTask, BridgeError> {
    match transfer_request(TransferCommand::Enqueue { draft }).await? {
        TransferOutput::Task { task } => Ok(task),
        _ => Err(invalid_transfer_response()),
    }
}

#[tauri::command]
pub async fn transfer_pick_upload_source(
    window: tauri::WebviewWindow,
) -> Result<Option<TransferLocalHandleGrant>, BridgeError> {
    pick_and_bind_transfer_local_handle(window, TransferLocalHandlePurpose::UploadSource).await
}

#[tauri::command]
pub async fn transfer_pick_download_destination(
    window: tauri::WebviewWindow,
) -> Result<Option<TransferLocalHandleGrant>, BridgeError> {
    pick_and_bind_transfer_local_handle(window, TransferLocalHandlePurpose::DownloadDestination)
        .await
}

async fn pick_and_bind_transfer_local_handle(
    window: tauri::WebviewWindow,
    purpose: TransferLocalHandlePurpose,
) -> Result<Option<TransferLocalHandleGrant>, BridgeError> {
    let picker = window
        .dialog()
        .file()
        .set_parent(&window)
        .set_title(match purpose {
            TransferLocalHandlePurpose::UploadSource => "选择要上传的文件",
            TransferLocalHandlePurpose::DownloadDestination => "选择下载保存位置",
        });
    let selected = tokio::task::spawn_blocking(move || match purpose {
        TransferLocalHandlePurpose::UploadSource => picker.blocking_pick_file(),
        TransferLocalHandlePurpose::DownloadDestination => picker.blocking_save_file(),
    })
    .await
    .map_err(|_| BridgeError::transport("native_file_picker_failed", true))?;
    let Some(selected) = selected else {
        return Ok(None);
    };
    let path = selected
        .into_path()
        .map_err(|_| BridgeError::transport("native_file_selection_not_local", false))?;
    let socket = runtime_socket_path().map_err(|error| error.into_bridge_error())?;
    request_transfer_local_handle(
        &socket,
        RequestEnvelope::transfer_local_handle(TransferLocalHandleBind { purpose, path }),
    )
    .await
    .map(Some)
    .map_err(Into::into)
}

#[tauri::command]
pub async fn transfer_list(query: TransferQuery) -> Result<TransferPage, BridgeError> {
    match transfer_request(TransferCommand::List { query }).await? {
        TransferOutput::Page { page } => Ok(page),
        _ => Err(invalid_transfer_response()),
    }
}

#[tauri::command]
pub async fn transfer_get(id: TransferId) -> Result<TransferTask, BridgeError> {
    match transfer_request(TransferCommand::Get { id }).await? {
        TransferOutput::Task { task } => Ok(task),
        _ => Err(invalid_transfer_response()),
    }
}

#[tauri::command]
pub async fn transfer_cancel(
    id: TransferId,
    expected_revision: u64,
) -> Result<TransferMutationResult, BridgeError> {
    transfer_mutation(TransferCommand::Cancel {
        id,
        expected_revision,
    })
    .await
}

#[tauri::command]
pub async fn transfer_retry(
    id: TransferId,
    expected_revision: u64,
) -> Result<TransferMutationResult, BridgeError> {
    transfer_mutation(TransferCommand::Retry {
        id,
        expected_revision,
    })
    .await
}

#[tauri::command]
pub async fn transfer_resolve_conflict(
    id: TransferId,
    expected_revision: u64,
    policy: ConflictPolicy,
) -> Result<TransferMutationResult, BridgeError> {
    transfer_mutation(TransferCommand::ResolveConflict {
        id,
        expected_revision,
        policy,
    })
    .await
}

async fn transfer_mutation(
    command: TransferCommand,
) -> Result<TransferMutationResult, BridgeError> {
    match transfer_request(command).await? {
        TransferOutput::Mutation { result } => Ok(result),
        _ => Err(invalid_transfer_response()),
    }
}

async fn transfer_request(command: TransferCommand) -> Result<TransferOutput, BridgeError> {
    let path = runtime_socket_path().map_err(|error| error.into_bridge_error())?;
    request_transfer(&path, RequestEnvelope::transfer(command))
        .await
        .map_err(Into::into)
}

fn invalid_transfer_response() -> BridgeError {
    BridgeError {
        kind: BridgeErrorKind::Protocol,
        code: "invalid_transfer_response".to_owned(),
        reason: "invalid_transfer_response".to_owned(),
        retryable: false,
    }
}

#[tauri::command]
pub async fn notes_list(query: NoteQuery) -> Result<NotePage, BridgeError> {
    match notes_request(NotesCommand::List { query }).await? {
        NotesOutput::Page(page) => Ok(page),
        _ => Err(invalid_notes_response()),
    }
}

#[tauri::command]
pub async fn notes_get(id: String) -> Result<NoteDocument, BridgeError> {
    match notes_request(NotesCommand::Get { id }).await? {
        NotesOutput::Document(document) => Ok(document),
        _ => Err(invalid_notes_response()),
    }
}

#[tauri::command]
pub async fn notes_upsert(
    intent: NoteWriteIntent,
    meta: NoteDraftMeta,
    body_markdown: String,
) -> Result<NoteMutationResult, BridgeError> {
    notes_write(intent, meta, body_markdown).await
}

#[tauri::command]
pub async fn notes_autosave(
    id: String,
    expected_revision: u64,
    meta: NoteDraftMeta,
    body_markdown: String,
) -> Result<NoteMutationResult, BridgeError> {
    notes_write(
        NoteWriteIntent::Save {
            id,
            expected_revision,
            autosave: true,
        },
        meta,
        body_markdown,
    )
    .await
}

#[tauri::command]
pub async fn notes_delete(
    id: String,
    expected_revision: u64,
) -> Result<NoteMutationResult, BridgeError> {
    notes_mutation(NotesCommand::Delete {
        id,
        expected_revision,
    })
    .await
}

#[tauri::command]
pub async fn notes_restore(
    id: String,
    expected_revision: u64,
) -> Result<NoteMutationResult, BridgeError> {
    notes_mutation(NotesCommand::Restore {
        id,
        expected_revision,
    })
    .await
}

#[tauri::command]
pub async fn notes_export(
    query: NoteQuery,
    format: NoteExportFormat,
) -> Result<NoteExport, BridgeError> {
    match notes_request(NotesCommand::Export { query, format }).await? {
        NotesOutput::Export(export) => Ok(export),
        _ => Err(invalid_notes_response()),
    }
}

async fn notes_write(
    intent: NoteWriteIntent,
    meta: NoteDraftMeta,
    body_markdown: String,
) -> Result<NoteMutationResult, BridgeError> {
    let inline = RequestEnvelope::notes(NotesCommand::WriteInline {
        intent: intent.clone(),
        meta: meta.clone(),
        body_markdown: body_markdown.clone(),
    });
    if request_fits_single_frame(&inline) {
        return notes_mutation_request(inline).await;
    }

    let expected_total_bytes = u32::try_from(body_markdown.len()).map_err(|_| BridgeError {
        kind: BridgeErrorKind::Protocol,
        code: "note_body_exceeds_4_mib".to_owned(),
        reason: "note_body_exceeds_4_mib".to_owned(),
        retryable: false,
    })?;
    let body_sha256 = format!("{:x}", Sha256::digest(body_markdown.as_bytes()));
    let begun = notes_mutation(NotesCommand::UploadBegin {
        intent: intent.clone(),
        meta,
        expected_total_bytes,
        body_sha256,
    })
    .await?;
    let NoteMutationResult::UploadBegun {
        upload_id,
        max_chunk_raw_bytes,
    } = begun
    else {
        return Err(invalid_notes_response());
    };
    if max_chunk_raw_bytes as usize != NOTE_CONTENT_CHUNK_BYTES {
        abort_upload(upload_id).await;
        return Err(invalid_notes_response());
    }

    let mut sequence = 0_u32;
    let mut offset = 0_u32;
    for chunk in body_markdown.as_bytes().chunks(NOTE_CONTENT_CHUNK_BYTES) {
        let result = notes_mutation(NotesCommand::UploadAppend {
            upload_id,
            sequence,
            offset,
            data_base64: STANDARD.encode(chunk),
        })
        .await;
        let accepted = match result {
            Ok(NoteMutationResult::UploadAccepted {
                upload_id: returned_id,
                next_sequence,
                next_offset,
            }) if returned_id == upload_id => (next_sequence, next_offset),
            Ok(_) => {
                abort_upload(upload_id).await;
                return Err(invalid_notes_response());
            }
            Err(error) => {
                abort_upload(upload_id).await;
                return Err(error);
            }
        };
        sequence = accepted.0;
        offset = accepted.1;
    }

    let committed = notes_mutation(NotesCommand::UploadCommit {
        upload_id,
        intent: intent.clone(),
    })
    .await;
    match committed {
        Ok(result) => match validate_upload_commit_result(&intent, result) {
            Ok(result) => Ok(result),
            Err(error) => {
                abort_upload(upload_id).await;
                Err(error)
            }
        },
        Err(error) => {
            abort_upload(upload_id).await;
            Err(error)
        }
    }
}

fn validate_upload_commit_result(
    intent: &NoteWriteIntent,
    result: NoteMutationResult,
) -> Result<NoteMutationResult, BridgeError> {
    let matches_intent = match (&result, intent) {
        (NoteMutationResult::Stored(note), NoteWriteIntent::Create) => note.validate().is_ok(),
        (NoteMutationResult::Stored(note), NoteWriteIntent::Save { id, .. }) => {
            note.id == *id && note.validate().is_ok()
        }
        (
            NoteMutationResult::Conflict {
                expected_revision,
                current,
            },
            NoteWriteIntent::Save {
                id,
                expected_revision: requested,
                ..
            },
        ) => expected_revision == requested && current.id == *id && current.validate().is_ok(),
        _ => false,
    };
    if matches_intent {
        Ok(result)
    } else {
        Err(invalid_notes_response())
    }
}

fn request_fits_single_frame(request: &RequestEnvelope) -> bool {
    serde_json::to_vec(request)
        .map(|encoded| encoded.len() <= MAX_FRAME_PAYLOAD_BYTES)
        .unwrap_or(false)
}

async fn abort_upload(upload_id: uuid::Uuid) {
    let _ = notes_mutation(NotesCommand::UploadAbort { upload_id }).await;
}

async fn notes_mutation(command: NotesCommand) -> Result<NoteMutationResult, BridgeError> {
    notes_mutation_request(RequestEnvelope::notes(command)).await
}

async fn notes_mutation_request(
    request: RequestEnvelope,
) -> Result<NoteMutationResult, BridgeError> {
    match notes_request_envelope(request).await? {
        NotesOutput::Mutation(result) => Ok(result),
        _ => Err(invalid_notes_response()),
    }
}

async fn notes_request(command: NotesCommand) -> Result<NotesOutput, BridgeError> {
    notes_request_envelope(RequestEnvelope::notes(command)).await
}

async fn notes_request_envelope(request: RequestEnvelope) -> Result<NotesOutput, BridgeError> {
    let path = runtime_socket_path().map_err(|error| error.into_bridge_error())?;
    request_notes(&path, request).await.map_err(Into::into)
}

fn invalid_notes_response() -> BridgeError {
    BridgeError {
        kind: BridgeErrorKind::Protocol,
        code: "invalid_notes_response".to_owned(),
        reason: "invalid_notes_response".to_owned(),
        retryable: false,
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct RuntimePathError {
    code: &'static str,
    retryable: bool,
}

impl RuntimePathError {
    const fn new(code: &'static str, retryable: bool) -> Self {
        Self { code, retryable }
    }

    fn into_bridge_error(self) -> BridgeError {
        BridgeError::transport(self.code, self.retryable)
    }
}

fn runtime_socket_path() -> Result<PathBuf, RuntimePathError> {
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or(RuntimePathError::new("runtime_dir_unavailable", true))?;
    if !runtime_dir.is_absolute() {
        return Err(RuntimePathError::new("runtime_dir_invalid", false));
    }
    validate_directory(&runtime_dir, 0o700, "runtime_dir_unavailable")?;
    let directory = runtime_dir.join("localdesk");
    validate_directory(&directory, 0o700, "appd_directory_unavailable")?;
    let socket = directory.join("appd.sock");
    let metadata = fs::symlink_metadata(&socket)
        .map_err(|_| RuntimePathError::new("appd_socket_unavailable", true))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_socket() {
        return Err(RuntimePathError::new("appd_socket_invalid", false));
    }
    if metadata.uid() != Uid::effective().as_raw() || metadata.permissions().mode() & 0o777 != 0o600
    {
        return Err(RuntimePathError::new("appd_socket_unsafe", false));
    }
    Ok(socket)
}

fn validate_directory(
    path: &Path,
    expected_mode: u32,
    reason: &'static str,
) -> Result<(), RuntimePathError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| RuntimePathError::new(reason, true))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(RuntimePathError::new(reason, false));
    }
    if metadata.uid() != Uid::effective().as_raw()
        || metadata.permissions().mode() & 0o777 != expected_mode
    {
        return Err(RuntimePathError::new(reason, false));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use localdesk_ipc::{DaemonError, ProtocolError, RequestBody};
    use localdesk_remote_core::{ProfileId, RemotePath};
    use localdesk_transfers::{
        BandwidthLimit, LocalFileHandle, RetryPolicy, TransferDirection, TransferDraftEndpoint,
        TransferId,
    };
    use std::future::Future;

    #[test]
    fn health_request_includes_all_wired_capabilities() {
        let RequestBody::Health(request) = health_request().body else {
            panic!("health request method");
        };

        assert!(
            request
                .requested_capabilities
                .contains(&NETWORK_SYSTEM_CAPABILITY.to_owned())
        );
        for capability in [
            REMOTE_SSH_CAPABILITY,
            REMOTE_SFTP_CAPABILITY,
            REMOTE_FTP_CAPABILITY,
            REMOTE_SMB_CAPABILITY,
            TRANSFERS_CAPABILITY,
            NOTES_CAPABILITY,
        ] {
            assert!(
                request
                    .requested_capabilities
                    .contains(&capability.to_owned())
            );
        }
        assert!(
            request
                .requested_capabilities
                .contains(&NETWORK_PER_APP_CAPABILITY.to_owned())
        );
        assert!(
            request
                .requested_capabilities
                .contains(&USAGE_FOREGROUND_CAPABILITY.to_owned())
        );
    }

    #[test]
    fn inline_notes_fast_path_uses_the_actual_serialized_frame_size() {
        let small = RequestEnvelope::notes(NotesCommand::WriteInline {
            intent: NoteWriteIntent::Create,
            meta: NoteDraftMeta {
                title: "small".to_owned(),
                diary_date: None,
                tags: Vec::new(),
                status: localdesk_domain::NoteStatus::Draft,
                pinned: false,
            },
            body_markdown: "body".to_owned(),
        });
        assert!(request_fits_single_frame(&small));

        let escaped = RequestEnvelope::notes(NotesCommand::WriteInline {
            intent: NoteWriteIntent::Create,
            meta: NoteDraftMeta {
                title: "escaped".to_owned(),
                diary_date: None,
                tags: Vec::new(),
                status: localdesk_domain::NoteStatus::Draft,
                pinned: false,
            },
            body_markdown: "\n".repeat(MAX_FRAME_PAYLOAD_BYTES),
        });
        assert!(!request_fits_single_frame(&escaped));
    }

    #[test]
    fn chunked_bridge_preserves_a_matching_cas_conflict() {
        let id = uuid::Uuid::from_u128(0x41).to_string();
        let intent = NoteWriteIntent::Save {
            id: id.clone(),
            expected_revision: 7,
            autosave: true,
        };
        let conflict = NoteMutationResult::Conflict {
            expected_revision: 7,
            current: localdesk_domain::NoteSummary {
                id,
                title: "newer".to_owned(),
                diary_date: None,
                tags: Vec::new(),
                status: localdesk_domain::NoteStatus::Draft,
                pinned: false,
                created_at_ms: 1,
                updated_at_ms: 2,
                deleted_at_ms: None,
                revision: 8,
                body_bytes: 0,
                body_sha256: format!("{:x}", Sha256::digest(b"")),
            },
        };

        assert_eq!(
            validate_upload_commit_result(&intent, conflict.clone()).expect("typed conflict"),
            conflict
        );
    }

    #[test]
    fn protocol_failure_is_typed_and_not_retryable() {
        let error = BridgeError::from(ClientError::Protocol(ProtocolError::Version));

        assert_eq!(error.kind, BridgeErrorKind::Protocol);
        assert_eq!(error.code, "appd_protocol_version_mismatch");
        assert_eq!(error.reason, error.code);
        assert!(!error.retryable);
    }

    #[test]
    fn daemon_failure_preserves_only_public_error_fields() {
        let error = BridgeError::from(ClientError::Daemon(DaemonError::new(
            "collector_unavailable",
            "helper_missing",
            true,
        )));

        assert_eq!(error.kind, BridgeErrorKind::Daemon);
        assert_eq!(error.code, "collector_unavailable");
        assert_eq!(error.reason, "helper_missing");
        assert!(error.retryable);
    }

    #[test]
    fn invalid_runtime_path_is_a_non_retryable_transport_failure() {
        let error = RuntimePathError::new("appd_socket_unsafe", false).into_bridge_error();

        assert_eq!(error.kind, BridgeErrorKind::Transport);
        assert_eq!(error.code, "appd_socket_unsafe");
        assert!(!error.retryable);
    }

    #[test]
    fn transfer_bridge_accepts_only_an_opaque_local_handle() {
        let handle = localdesk_transfers::LocalFileHandle::new();
        let draft = TransferDraft {
            id: TransferId::new(),
            source: TransferDraftEndpoint::Local { handle },
            destination: TransferDraftEndpoint::Remote {
                profile_id: ProfileId::new(),
                path: RemotePath::new("/destination.bin").expect("remote path"),
            },
            direction: TransferDirection::Upload,
            expected_source: None,
            expected_destination: None,
            retry_policy: RetryPolicy::default(),
            bandwidth_limit: BandwidthLimit::unlimited(),
            conflict_policy: ConflictPolicy::Fail,
        };
        let mut encoded = serde_json::to_value(draft).expect("serialize transfer draft");
        let local = encoded
            .get_mut("source")
            .and_then(serde_json::Value::as_object_mut)
            .expect("local endpoint object");
        assert_eq!(local.len(), 2);
        assert!(local.contains_key("kind"));
        assert!(local.contains_key("handle"));

        local.insert(
            "path".to_owned(),
            serde_json::Value::String("/tmp/not-authorized".to_owned()),
        );
        assert!(serde_json::from_value::<TransferDraft>(encoded).is_err());
    }

    #[test]
    fn native_picker_commands_take_no_frontend_path_and_return_no_path() {
        fn assert_picker_signature<F>(_: fn(tauri::WebviewWindow) -> F)
        where
            F: Future<Output = Result<Option<TransferLocalHandleGrant>, BridgeError>>,
        {
        }

        assert_picker_signature(transfer_pick_upload_source);
        assert_picker_signature(transfer_pick_download_destination);

        let grant = TransferLocalHandleGrant {
            handle: LocalFileHandle::new(),
            purpose: TransferLocalHandlePurpose::UploadSource,
            display_name: "source.bin".to_owned(),
            size_bytes: Some(12),
        };
        let encoded = serde_json::to_value(grant).expect("serialize local handle grant");
        let object = encoded.as_object().expect("grant object");
        assert_eq!(object.len(), 4);
        assert!(object.contains_key("handle"));
        assert!(object.contains_key("purpose"));
        assert!(object.contains_key("display_name"));
        assert!(object.contains_key("size_bytes"));
        assert!(!object.contains_key("path"));
    }
}
