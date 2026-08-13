use localdesk_domain::{CapabilityRuntime, CapabilityRuntimeState};
use localdesk_ipc::{
    ClientError, ProtocolError, RequestEnvelope, ResponseBody, ResponseEnvelope, ServerConfig,
    TransferLocalHandleBind, TransferLocalHandleProvider, TransferPageEnd, TransferPageStart,
    TransferProvider, TransferTaskChunk, WIRE_PROTOCOL_VERSION, WireBudget, read_json,
    request_transfer, request_transfer_local_handle, serve, write_json,
};
use localdesk_remote_core::{
    CapabilityMatrix, CapabilityStatus, FILE_OPERATIONS, ObjectIdentity, OperationCapability,
    ProfileId, RemotePath, RemoteProtocol,
};
use localdesk_transfers::{
    BandwidthLimit, ConflictPolicy, LocalFileHandle, MAX_TRANSFER_ETAG_BYTES,
    MAX_TRANSFER_PAGE_TASKS, RetryPolicy, TRANSFER_PUBLIC_SCHEMA_VERSION, TransferCommand,
    TransferDirection, TransferDraft, TransferDraftEndpoint, TransferFeatureSet, TransferId,
    TransferLocalHandleGrant, TransferLocalHandlePurpose, TransferMutationResult, TransferOutput,
    TransferPage, TransferQuery, TransferStateKind, TransferTask,
};
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

fn capabilities() -> CapabilityMatrix {
    CapabilityMatrix::complete(FILE_OPERATIONS.iter().map(|operation| OperationCapability {
        operation: *operation,
        status: CapabilityStatus::Supported,
    }))
    .expect("capabilities")
}

fn draft(id: TransferId) -> TransferDraft {
    TransferDraft {
        id,
        source: TransferDraftEndpoint::Local {
            handle: LocalFileHandle::new(),
        },
        destination: TransferDraftEndpoint::Remote {
            profile_id: ProfileId::from_uuid(Uuid::from_u128(7)),
            path: RemotePath::new("/fixture.bin").expect("path"),
        },
        direction: TransferDirection::Upload,
        expected_source: None,
        expected_destination: None,
        retry_policy: RetryPolicy::default(),
        bandwidth_limit: BandwidthLimit::unlimited(),
        conflict_policy: ConflictPolicy::Fail,
    }
}

fn task(id: TransferId, revision: u64) -> TransferTask {
    let mut task = draft(id)
        .into_task(
            RemoteProtocol::Sftp,
            TransferFeatureSet::from_adapter(
                TransferDirection::Upload,
                RemoteProtocol::Sftp,
                &capabilities(),
            ),
            1,
        )
        .expect("task");
    task.revision = revision;
    task
}

fn query() -> TransferQuery {
    TransferQuery {
        limit: MAX_TRANSFER_PAGE_TASKS,
        offset: 0,
        states: vec![TransferStateKind::Queued],
        direction: None,
        profile_id: None,
    }
}

async fn spawn_server(
    provider: Option<TransferProvider>,
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
    let mut config = ServerConfig::new("fixture", Arc::new(runtime));
    if let Some(provider) = provider {
        config = config.with_transfer_provider(provider);
    }
    let server = tokio::spawn(serve(listener, config, shutdown_rx));
    (directory, path, shutdown_tx, server)
}

#[tokio::test]
async fn v12_roundtrips_all_commands_multiframe_list_and_typed_conflict() {
    let listed = (1..=u128::from(MAX_TRANSFER_PAGE_TASKS))
        .map(|id| task(TransferId::from_uuid(Uuid::from_u128(id)), 0))
        .collect::<Vec<_>>();
    let provider: TransferProvider = Arc::new({
        let listed = listed.clone();
        move |command| {
            let listed = listed.clone();
            Box::pin(async move {
                Ok(match command {
                    TransferCommand::Enqueue { draft } => TransferOutput::Task {
                        task: task(draft.id, 0),
                    },
                    TransferCommand::List { query } => TransferOutput::Page {
                        page: TransferPage {
                            query,
                            tasks: listed,
                            has_more: false,
                            next_offset: None,
                        },
                    },
                    TransferCommand::Get { id } => TransferOutput::Task { task: task(id, 3) },
                    TransferCommand::Cancel {
                        id,
                        expected_revision,
                    } if expected_revision == 99 => TransferOutput::Mutation {
                        result: TransferMutationResult::Conflict {
                            expected_revision,
                            current: task(id, 100),
                        },
                    },
                    TransferCommand::Cancel {
                        id,
                        expected_revision,
                    }
                    | TransferCommand::Retry {
                        id,
                        expected_revision,
                    }
                    | TransferCommand::ResolveConflict {
                        id,
                        expected_revision,
                        ..
                    } => TransferOutput::Mutation {
                        result: TransferMutationResult::Updated {
                            task: task(id, expected_revision + 1),
                        },
                    },
                })
            })
        }
    });
    let (_directory, path, shutdown_tx, server) = spawn_server(Some(provider)).await;
    let id = TransferId::from_uuid(Uuid::from_u128(0x99));

    for command in [
        TransferCommand::Enqueue { draft: draft(id) },
        TransferCommand::Get { id },
        TransferCommand::Cancel {
            id,
            expected_revision: 3,
        },
        TransferCommand::Retry {
            id,
            expected_revision: 4,
        },
        TransferCommand::ResolveConflict {
            id,
            expected_revision: 5,
            policy: ConflictPolicy::Overwrite,
        },
    ] {
        request_transfer(&path, RequestEnvelope::transfer(command))
            .await
            .expect("single-frame transfer result");
    }

    let page = request_transfer(
        &path,
        RequestEnvelope::transfer(TransferCommand::List { query: query() }),
    )
    .await
    .expect("multi-frame page");
    assert!(matches!(page, TransferOutput::Page { page } if page.tasks.len() == 64));

    let conflict = request_transfer(
        &path,
        RequestEnvelope::transfer(TransferCommand::Cancel {
            id,
            expected_revision: 99,
        }),
    )
    .await
    .expect("typed conflict");
    assert!(matches!(
        conflict,
        TransferOutput::Mutation {
            result: TransferMutationResult::Conflict {
                expected_revision: 99,
                current,
            },
        } if current.revision == 100
    ));

    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}

#[tokio::test]
async fn missing_provider_is_a_typed_retryable_daemon_error() {
    let (_directory, path, shutdown_tx, server) = spawn_server(None).await;
    let error = request_transfer(
        &path,
        RequestEnvelope::transfer(TransferCommand::Get {
            id: TransferId::new(),
        }),
    )
    .await
    .expect_err("provider unavailable");
    assert!(matches!(
        error,
        ClientError::Daemon(error)
            if error.code == "transfer_provider_unavailable" && error.retryable
    ));
    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}

#[tokio::test]
async fn rust_side_local_handle_binding_returns_no_path_and_validates_both_peers() {
    let directory = tempdir().expect("socket directory");
    let path = directory.path().join("appd.sock");
    let listener = UnixListener::bind(&path).expect("listener");
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let provider: TransferLocalHandleProvider = Arc::new(|bind| {
        Box::pin(async move {
            assert_eq!(bind.purpose, TransferLocalHandlePurpose::UploadSource);
            Ok(TransferLocalHandleGrant {
                handle: LocalFileHandle::new(),
                purpose: bind.purpose,
                display_name: "fixture.bin".to_owned(),
                size_bytes: Some(7),
            })
        })
    });
    let config = ServerConfig::new("fixture", Arc::new(runtime))
        .with_transfer_local_handle_provider(provider);
    let server = tokio::spawn(serve(listener, config, shutdown_rx));
    let grant = request_transfer_local_handle(
        &path,
        RequestEnvelope::transfer_local_handle(TransferLocalHandleBind {
            purpose: TransferLocalHandlePurpose::UploadSource,
            path: PathBuf::from("/tmp/fixture.bin"),
        }),
    )
    .await
    .expect("bound handle");
    let encoded = serde_json::to_value(&grant).expect("grant json");
    assert_eq!(grant.display_name, "fixture.bin");
    assert!(encoded.get("path").is_none());

    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");

    let (_directory, path, shutdown_tx, server) = spawn_server(None).await;
    let error = request_transfer_local_handle(
        &path,
        RequestEnvelope::transfer_local_handle(TransferLocalHandleBind {
            purpose: TransferLocalHandlePurpose::DownloadDestination,
            path: PathBuf::from("/tmp/destination.bin"),
        }),
    )
    .await
    .expect_err("missing bind provider");
    assert!(matches!(
        error,
        ClientError::Daemon(error)
            if error.code == "transfer_local_handle_provider_unavailable" && error.retryable
    ));
    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}

#[tokio::test]
async fn server_rejects_oversized_task_and_page_before_wire() {
    let id = TransferId::new();
    let provider: TransferProvider = Arc::new(move |command| {
        Box::pin(async move {
            let mut invalid = task(command.task_id().unwrap_or(id), 0);
            invalid.expected_source = Some(ObjectIdentity {
                size_bytes: None,
                modified_at_unix_ms: None,
                etag: Some("x".repeat(MAX_TRANSFER_ETAG_BYTES + 1)),
            });
            Ok(match command {
                TransferCommand::List { query } => TransferOutput::Page {
                    page: TransferPage {
                        query,
                        tasks: vec![invalid; usize::from(MAX_TRANSFER_PAGE_TASKS) + 1],
                        has_more: false,
                        next_offset: None,
                    },
                },
                _ => TransferOutput::Task { task: invalid },
            })
        })
    });
    let (_directory, path, shutdown_tx, server) = spawn_server(Some(provider)).await;
    for command in [
        TransferCommand::Get { id },
        TransferCommand::List { query: query() },
    ] {
        let error = request_transfer(&path, RequestEnvelope::transfer(command))
            .await
            .expect_err("invalid provider result");
        assert!(matches!(
            error,
            ClientError::Daemon(error) if error.code == "transfer_result_invalid"
        ));
    }
    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}

async fn spawn_raw_page_server(
    mutate: impl FnOnce(&mut ResponseEnvelope, &mut ResponseEnvelope) + Send + 'static,
) -> (tempfile::TempDir, PathBuf, tokio::task::JoinHandle<()>) {
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
        let RequestEnvelope {
            request_id,
            body: localdesk_ipc::RequestBody::Transfer(TransferCommand::List { query }),
            ..
        } = request
        else {
            panic!("list request")
        };
        let snapshot_id = Uuid::new_v4();
        let mut start = ResponseEnvelope {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id,
            sequence: 0,
            snapshot_id: Some(snapshot_id),
            body: ResponseBody::TransferPageStart(Box::new(TransferPageStart {
                schema_version: TRANSFER_PUBLIC_SCHEMA_VERSION,
                query: query.clone(),
                total_tasks: 1,
                data_frame_count: 1,
                has_more: false,
                next_offset: None,
            })),
        };
        let mut end = ResponseEnvelope {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id,
            sequence: 1,
            snapshot_id: Some(snapshot_id),
            body: ResponseBody::TransferPageEnd(TransferPageEnd {
                schema_version: TRANSFER_PUBLIC_SCHEMA_VERSION,
                query,
                total_tasks: 1,
                data_frame_count: 1,
                has_more: false,
                next_offset: None,
            }),
        };
        mutate(&mut start, &mut end);
        write_json(&mut stream, &start, deadline)
            .await
            .expect("start");
        write_json(&mut stream, &end, deadline).await.expect("end");
    });
    (directory, path, server)
}

async fn spawn_raw_transfer_frames(
    build: impl FnOnce(Uuid, Uuid, TransferQuery) -> Vec<ResponseEnvelope> + Send + 'static,
) -> (tempfile::TempDir, PathBuf, tokio::task::JoinHandle<()>) {
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
        let RequestEnvelope {
            request_id,
            body: localdesk_ipc::RequestBody::Transfer(TransferCommand::List { query }),
            ..
        } = request
        else {
            panic!("list request")
        };
        let snapshot_id = Uuid::new_v4();
        for frame in build(request_id, snapshot_id, query) {
            write_json(&mut stream, &frame, deadline)
                .await
                .expect("raw transfer frame");
        }
    });
    (directory, path, server)
}

fn raw_start(
    request_id: Uuid,
    snapshot_id: Uuid,
    query: TransferQuery,
    total_tasks: u32,
    data_frame_count: u32,
) -> ResponseEnvelope {
    ResponseEnvelope {
        protocol_version: WIRE_PROTOCOL_VERSION,
        request_id,
        sequence: 0,
        snapshot_id: Some(snapshot_id),
        body: ResponseBody::TransferPageStart(Box::new(TransferPageStart {
            schema_version: TRANSFER_PUBLIC_SCHEMA_VERSION,
            query,
            total_tasks,
            data_frame_count,
            has_more: false,
            next_offset: None,
        })),
    }
}

fn raw_chunk(
    request_id: Uuid,
    snapshot_id: Uuid,
    sequence: u32,
    records: Vec<TransferTask>,
) -> ResponseEnvelope {
    ResponseEnvelope {
        protocol_version: WIRE_PROTOCOL_VERSION,
        request_id,
        sequence,
        snapshot_id: Some(snapshot_id),
        body: ResponseBody::TransferTaskChunk(TransferTaskChunk { records }),
    }
}

fn raw_end(
    request_id: Uuid,
    snapshot_id: Uuid,
    sequence: u32,
    query: TransferQuery,
    total_tasks: u32,
    data_frame_count: u32,
) -> ResponseEnvelope {
    ResponseEnvelope {
        protocol_version: WIRE_PROTOCOL_VERSION,
        request_id,
        sequence,
        snapshot_id: Some(snapshot_id),
        body: ResponseBody::TransferPageEnd(TransferPageEnd {
            schema_version: TRANSFER_PUBLIC_SCHEMA_VERSION,
            query,
            total_tasks,
            data_frame_count,
            has_more: false,
            next_offset: None,
        }),
    }
}

#[tokio::test]
async fn client_rejects_malformed_transfer_sequence_and_counts() {
    let (_directory, path, server) = spawn_raw_page_server(|_, end| end.sequence = 2).await;
    let error = request_transfer(
        &path,
        RequestEnvelope::transfer(TransferCommand::List { query: query() }),
    )
    .await
    .expect_err("sequence mismatch");
    assert!(matches!(
        error,
        ClientError::Protocol(ProtocolError::Sequence)
    ));
    server.await.expect("server");
}

#[tokio::test]
async fn client_rejects_an_empty_transfer_chunk() {
    let (_directory, path, server) = spawn_raw_transfer_frames(|request_id, snapshot_id, query| {
        vec![
            raw_start(request_id, snapshot_id, query, 1, 1),
            raw_chunk(request_id, snapshot_id, 1, Vec::new()),
        ]
    })
    .await;
    let error = request_transfer(
        &path,
        RequestEnvelope::transfer(TransferCommand::List { query: query() }),
    )
    .await
    .expect_err("empty chunk");
    assert!(matches!(
        error,
        ClientError::Protocol(ProtocolError::InvalidChunk)
    ));
    server.await.expect("server");
}

#[tokio::test]
async fn client_rejects_transfer_count_above_declaration_and_page_maximum() {
    let (_directory, path, server) = spawn_raw_transfer_frames(|request_id, snapshot_id, query| {
        let records = (1..=65_u128)
            .map(|id| task(TransferId::from_uuid(Uuid::from_u128(id)), 0))
            .collect::<Vec<_>>();
        vec![
            raw_start(request_id, snapshot_id, query, 64, 3),
            raw_chunk(request_id, snapshot_id, 1, records[..32].to_vec()),
            raw_chunk(request_id, snapshot_id, 2, records[32..64].to_vec()),
            raw_chunk(request_id, snapshot_id, 3, records[64..].to_vec()),
        ]
    })
    .await;
    let error = request_transfer(
        &path,
        RequestEnvelope::transfer(TransferCommand::List { query: query() }),
    )
    .await
    .expect_err("page count overflow");
    assert!(matches!(
        error,
        ClientError::Protocol(ProtocolError::InvalidChunk)
    ));
    server.await.expect("server");
}

#[tokio::test]
async fn client_rejects_contradictory_transfer_start_totals() {
    let (_directory, path, server) = spawn_raw_transfer_frames(|request_id, snapshot_id, query| {
        vec![raw_start(request_id, snapshot_id, query, 0, 1)]
    })
    .await;
    let error = request_transfer(
        &path,
        RequestEnvelope::transfer(TransferCommand::List { query: query() }),
    )
    .await
    .expect_err("contradictory start");
    assert!(matches!(
        error,
        ClientError::Protocol(ProtocolError::InvalidStart)
    ));
    server.await.expect("server");
}

#[tokio::test]
async fn client_rejects_transfer_end_count_mismatch() {
    let (_directory, path, server) = spawn_raw_transfer_frames(|request_id, snapshot_id, query| {
        vec![
            raw_start(request_id, snapshot_id, query.clone(), 1, 1),
            raw_chunk(request_id, snapshot_id, 1, vec![task(TransferId::new(), 0)]),
            raw_end(request_id, snapshot_id, 2, query, 2, 1),
        ]
    })
    .await;
    let error = request_transfer(
        &path,
        RequestEnvelope::transfer(TransferCommand::List { query: query() }),
    )
    .await
    .expect_err("end count mismatch");
    assert!(matches!(
        error,
        ClientError::Protocol(ProtocolError::InvalidEnd)
    ));
    server.await.expect("server");
}

#[tokio::test]
async fn client_rejects_premature_transfer_stream_eof() {
    let (_directory, path, server) = spawn_raw_transfer_frames(|request_id, snapshot_id, query| {
        vec![raw_start(request_id, snapshot_id, query, 1, 1)]
    })
    .await;
    let error = request_transfer(
        &path,
        RequestEnvelope::transfer(TransferCommand::List { query: query() }),
    )
    .await
    .expect_err("premature eof");
    assert!(matches!(
        error,
        ClientError::Protocol(ProtocolError::MissingTerminal)
    ));
    server.await.expect("server");
}

#[tokio::test]
async fn client_rejects_mismatched_snapshot_identity() {
    let (_directory, path, server) = spawn_raw_page_server(|_, end| {
        end.snapshot_id = Some(Uuid::new_v4());
    })
    .await;
    let error = request_transfer(
        &path,
        RequestEnvelope::transfer(TransferCommand::List { query: query() }),
    )
    .await
    .expect_err("snapshot mismatch");
    assert!(matches!(
        error,
        ClientError::Protocol(ProtocolError::SnapshotIdentity)
    ));
    server.await.expect("server");
}

#[tokio::test]
async fn client_rejects_single_task_with_the_wrong_identity() {
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
        write_json(
            &mut stream,
            &ResponseEnvelope {
                protocol_version: WIRE_PROTOCOL_VERSION,
                request_id: request.request_id,
                sequence: 0,
                snapshot_id: None,
                body: ResponseBody::Transfer(Box::new(TransferOutput::Task {
                    task: task(TransferId::new(), 0),
                })),
            },
            deadline,
        )
        .await
        .expect("response");
    });
    let error = request_transfer(
        &path,
        RequestEnvelope::transfer(TransferCommand::Get {
            id: TransferId::new(),
        }),
    )
    .await
    .expect_err("wrong task id");
    assert!(matches!(
        error,
        ClientError::Protocol(ProtocolError::UnexpectedBody)
    ));
    server.await.expect("server");
}
