use localdesk_domain::{
    CapabilityRuntime, CapabilityRuntimeState, MAX_NOTE_BODY_BYTES, NOTE_CONTENT_CHUNK_BYTES,
    NOTES_SCHEMA_VERSION, NoteDeletedFilter, NoteDocument, NoteDraftMeta, NoteExport,
    NoteExportFormat, NoteMutationResult, NotePage, NoteQuery, NoteSort, NoteStatus, NoteSummary,
    NoteWriteIntent, NotesCommand, NotesOutput,
};
use localdesk_ipc::{
    ClientError, NotesContentChunk, NotesContentEnd, NotesContentKind, NotesContentStart,
    NotesProvider, RequestEnvelope, ResponseBody, ResponseEnvelope, ServerConfig,
    WIRE_PROTOCOL_VERSION, WireBudget, read_json, request_notes, serve, write_json,
};
use sha2::{Digest, Sha256};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tempfile::tempdir;
use tokio::{net::UnixListener, sync::watch, time::Instant};
use uuid::Uuid;

fn runtime() -> CapabilityRuntime {
    CapabilityRuntime::new(
        CapabilityRuntimeState::healthy("appd_online"),
        CapabilityRuntimeState::degraded("telemetry_warming_up"),
        CapabilityRuntimeState::degraded("network_warming_up"),
        CapabilityRuntimeState::unsupported("per_app_unavailable"),
        CapabilityRuntimeState::degraded("usage_warming_up"),
    )
}

fn query() -> NoteQuery {
    NoteQuery {
        search: None,
        diary_date_from: None,
        diary_date_to: None,
        tags: Vec::new(),
        status: None,
        deleted: NoteDeletedFilter::Exclude,
        sort: NoteSort::UpdatedDesc,
        limit: 64,
        offset: 0,
    }
}

fn summary(id: Uuid, body: &str) -> NoteSummary {
    NoteSummary {
        id: id.to_string(),
        title: "fixture".to_owned(),
        diary_date: Some("2026-08-09".to_owned()),
        tags: vec!["test".to_owned()],
        status: NoteStatus::Active,
        pinned: false,
        created_at_ms: 1,
        updated_at_ms: 2,
        deleted_at_ms: None,
        revision: 1,
        body_bytes: u32::try_from(body.len()).expect("body length"),
        body_sha256: format!("{:x}", Sha256::digest(body.as_bytes())),
    }
}

async fn spawn_server(
    provider: NotesProvider,
) -> (
    tempfile::TempDir,
    PathBuf,
    watch::Sender<bool>,
    tokio::task::JoinHandle<Result<(), localdesk_ipc::ServerError>>,
) {
    let directory = tempdir().expect("socket directory");
    let path = directory.path().join("appd.sock");
    let listener = UnixListener::bind(&path).expect("listener");
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let config = ServerConfig::new("fixture", Arc::new(runtime)).with_notes_provider(provider);
    let server = tokio::spawn(serve(listener, config, shutdown_rx));
    (directory, path, shutdown_tx, server)
}

#[tokio::test]
async fn mutation_and_page_results_remain_command_typed() {
    let stored = summary(Uuid::from_u128(1), "body");
    let provider: NotesProvider = Arc::new({
        let stored = stored.clone();
        move |command| {
            let stored = stored.clone();
            Box::pin(async move {
                Ok(match command {
                    NotesCommand::List { query } => NotesOutput::Page(NotePage {
                        query,
                        notes: vec![stored],
                        has_more: false,
                        next_offset: None,
                    }),
                    NotesCommand::WriteInline { .. } => {
                        NotesOutput::Mutation(NoteMutationResult::Stored(stored))
                    }
                    _ => unreachable!("fixture command"),
                })
            })
        }
    });
    let (_directory, path, shutdown_tx, server) = spawn_server(provider).await;

    let page = request_notes(
        &path,
        RequestEnvelope::notes(NotesCommand::List { query: query() }),
    )
    .await
    .expect("page");
    assert!(matches!(page, NotesOutput::Page(NotePage { notes, .. }) if notes.len() == 1));

    let stored_result = request_notes(
        &path,
        RequestEnvelope::notes(NotesCommand::WriteInline {
            intent: NoteWriteIntent::Create,
            meta: NoteDraftMeta {
                title: "fixture".to_owned(),
                diary_date: None,
                tags: Vec::new(),
                status: NoteStatus::Draft,
                pinned: false,
            },
            body_markdown: "body".to_owned(),
        }),
    )
    .await
    .expect("stored");
    assert!(matches!(
        stored_result,
        NotesOutput::Mutation(NoteMutationResult::Stored(_))
    ));

    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}

#[tokio::test]
async fn upload_commit_preserves_a_typed_cas_conflict() {
    let note_id = Uuid::from_u128(0x31);
    let mut current = summary(note_id, "newer body");
    current.revision = 2;
    let expected = NoteMutationResult::Conflict {
        expected_revision: 1,
        current,
    };
    let provider: NotesProvider = Arc::new({
        let expected = expected.clone();
        move |command| {
            let expected = expected.clone();
            Box::pin(async move {
                assert!(matches!(command, NotesCommand::UploadCommit { .. }));
                Ok(NotesOutput::Mutation(expected))
            })
        }
    });
    let (_directory, path, shutdown_tx, server) = spawn_server(provider).await;

    let actual = request_notes(
        &path,
        RequestEnvelope::notes(NotesCommand::UploadCommit {
            upload_id: Uuid::from_u128(0x32),
            intent: NoteWriteIntent::Save {
                id: note_id.to_string(),
                expected_revision: 1,
                autosave: true,
            },
        }),
    )
    .await
    .expect("typed conflict");

    assert_eq!(actual, NotesOutput::Mutation(expected));
    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}

#[tokio::test]
async fn four_mib_utf8_document_roundtrip_uses_exactly_94_data_frames() {
    let mut bytes = "界".repeat(MAX_NOTE_BODY_BYTES / 3).into_bytes();
    bytes.extend(std::iter::repeat_n(b'x', MAX_NOTE_BODY_BYTES - bytes.len()));
    let body = String::from_utf8(bytes).expect("UTF-8 body");
    assert_eq!(body.len(), MAX_NOTE_BODY_BYTES);
    assert_eq!(body.len().div_ceil(NOTE_CONTENT_CHUNK_BYTES), 94);
    let expected = NoteDocument {
        summary: summary(Uuid::from_u128(2), &body),
        body_markdown: body,
    };
    let provider: NotesProvider = Arc::new({
        let expected = expected.clone();
        move |_| {
            let expected = expected.clone();
            Box::pin(async move { Ok(NotesOutput::Document(expected)) })
        }
    });
    let (_directory, path, shutdown_tx, server) = spawn_server(provider).await;

    let actual = request_notes(
        &path,
        RequestEnvelope::notes(NotesCommand::Get {
            id: expected.summary.id.clone(),
        }),
    )
    .await
    .expect("document");
    assert_eq!(actual, NotesOutput::Document(expected));

    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}

#[tokio::test]
async fn both_export_formats_can_use_the_scoped_transport_budget() {
    let target_bytes = 9 * 1024 * 1024 + 1_024;
    for format in [NoteExportFormat::Json, NoteExportFormat::Markdown] {
        let content = match format {
            NoteExportFormat::Json => format!("\"{}\"", "x".repeat(target_bytes - 2)),
            NoteExportFormat::Markdown => "x".repeat(target_bytes),
        };
        let expected = NoteExport {
            format,
            content_bytes: u32::try_from(content.len()).expect("export length"),
            content_sha256: format!("{:x}", Sha256::digest(content.as_bytes())),
            content,
        };
        let provider: NotesProvider = Arc::new({
            let expected = expected.clone();
            move |_| {
                let expected = expected.clone();
                Box::pin(async move { Ok(NotesOutput::Export(expected)) })
            }
        });
        let (_directory, path, shutdown_tx, server) = spawn_server(provider).await;

        let actual = request_notes(
            &path,
            RequestEnvelope::notes(NotesCommand::Export {
                query: query(),
                format,
            }),
        )
        .await
        .expect("export");
        assert_eq!(actual, NotesOutput::Export(expected));

        shutdown_tx.send(true).expect("shutdown");
        server.await.expect("join").expect("serve");
    }
}

#[tokio::test]
async fn client_rejects_a_content_chunk_with_the_wrong_offset() {
    let body = "hello";
    let expected = summary(Uuid::from_u128(3), body);
    let directory = tempdir().expect("socket directory");
    let path = directory.path().join("appd.sock");
    let listener = UnixListener::bind(&path).expect("listener");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut budget = WireBudget::new(1, 65_540);
        let request: RequestEnvelope = read_json(&mut stream, deadline, &mut budget)
            .await
            .expect("request");
        let snapshot_id = Uuid::from_u128(4);
        let start = NotesContentStart {
            schema_version: NOTES_SCHEMA_VERSION,
            kind: NotesContentKind::Document,
            note: Some(expected.clone()),
            total_bytes: body.len() as u32,
            content_sha256: expected.body_sha256.clone(),
            data_frame_count: 1,
        };
        write_json(
            &mut stream,
            &ResponseEnvelope {
                protocol_version: WIRE_PROTOCOL_VERSION,
                request_id: request.request_id,
                sequence: 0,
                snapshot_id: Some(snapshot_id),
                body: ResponseBody::NotesContentStart(Box::new(start)),
            },
            deadline,
        )
        .await
        .expect("start");
        write_json(
            &mut stream,
            &ResponseEnvelope {
                protocol_version: WIRE_PROTOCOL_VERSION,
                request_id: request.request_id,
                sequence: 1,
                snapshot_id: Some(snapshot_id),
                body: ResponseBody::NotesContentChunk(NotesContentChunk {
                    offset: 1,
                    raw_bytes: body.len() as u32,
                    data_base64: "aGVsbG8=".to_owned(),
                }),
            },
            deadline,
        )
        .await
        .expect("chunk");
        write_json(
            &mut stream,
            &ResponseEnvelope {
                protocol_version: WIRE_PROTOCOL_VERSION,
                request_id: request.request_id,
                sequence: 2,
                snapshot_id: Some(snapshot_id),
                body: ResponseBody::NotesContentEnd(NotesContentEnd {
                    schema_version: NOTES_SCHEMA_VERSION,
                    kind: NotesContentKind::Document,
                    total_bytes: body.len() as u32,
                    content_sha256: expected.body_sha256,
                    data_frame_count: 1,
                }),
            },
            deadline,
        )
        .await
        .expect("end");
    });

    let error = request_notes(
        &path,
        RequestEnvelope::notes(NotesCommand::Get {
            id: Uuid::from_u128(3).to_string(),
        }),
    )
    .await
    .expect_err("wrong offset");
    assert!(matches!(error, ClientError::Protocol(_)));
    server.await.expect("join");
}
