use localdesk_domain::{
    ApplicationSample, CapabilityRuntime, CapabilityRuntimeState, GroupingResolution, MetricValue,
    SystemFdSample, TELEMETRY_SCHEMA_VERSION, TELEMETRY_SCOPE_FULL_CGROUP,
    TELEMETRY_SCOPE_SAME_EUID, TELEMETRY_SCOPE_SYSTEM, TelemetryFreshness, TelemetrySnapshot,
    TelemetryStatus,
};
use localdesk_ipc::{
    ApplicationChunk, ClientError, DaemonError, RequestEnvelope, ResponseBody, ResponseEnvelope,
    ServerConfig, SnapshotEnd, SnapshotProvider, SnapshotStart, WIRE_PROTOCOL_VERSION, WireBudget,
    read_json, request_telemetry_snapshot, serve, write_json,
};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tempfile::tempdir;
use tokio::{
    io::AsyncWriteExt,
    net::UnixListener,
    sync::{Barrier, Semaphore, watch},
    time::Instant,
};
use uuid::Uuid;

fn runtime() -> CapabilityRuntime {
    CapabilityRuntime::new(
        CapabilityRuntimeState::healthy("appd_online"),
        CapabilityRuntimeState::healthy("snapshot_available"),
        CapabilityRuntimeState::healthy("network_available"),
        CapabilityRuntimeState::unsupported("per_app_unavailable"),
        CapabilityRuntimeState::degraded("usage_warming_up"),
    )
}

fn application(index: usize, label_bytes: usize) -> ApplicationSample {
    ApplicationSample {
        application_key: format!("app-{index}"),
        desktop_entry_id: None,
        display_label: "x".repeat(label_bytes),
        grouping_resolution: GroupingResolution::Unknown,
        process_count: 1,
        process_scope: TELEMETRY_SCOPE_SAME_EUID.to_owned(),
        cgroup_scope: TELEMETRY_SCOPE_FULL_CGROUP.to_owned(),
        cpu_percent_total_capacity_sum: MetricValue::known(1.0),
        rss_sum_bytes: MetricValue::known(4_096),
        pss_sum_bytes: MetricValue::known(3_072),
        fd_used_sum: MetricValue::known(1),
        fd_soft_limit_sum: MetricValue::known(100),
        fd_percent_of_attributed_sum: MetricValue::known(50.0),
        fd_percent_of_soft_limit_sum: MetricValue::known(1.0),
        fd_max_process_percent_of_soft_limit: MetricValue::known(90.0),
        cgroup_cpu_percent_total_capacity: MetricValue::known(0.5),
        memory_current_bytes: MetricValue::known(8_192),
        cgroup_process_count: MetricValue::known(1),
    }
}

fn snapshot(record_count: usize, label_bytes: usize) -> TelemetrySnapshot {
    TelemetrySnapshot {
        schema_version: TELEMETRY_SCHEMA_VERSION,
        snapshot_id: Uuid::new_v4(),
        captured_at_unix_ms: Some(1),
        sample_interval_ms: Some(1_000),
        logical_cpu_count: Some(4),
        freshness: TelemetryFreshness::Fresh,
        status: TelemetryStatus::Complete,
        reason: "complete".to_owned(),
        retryable: false,
        scope: "same_euid".to_owned(),
        last_success_at_unix_ms: Some(1),
        permission_denied_counts: Vec::new(),
        issues: Vec::new(),
        system_fd: system_fd(),
        applications: (0..record_count)
            .map(|index| application(index, label_bytes))
            .collect(),
    }
}

fn system_fd() -> SystemFdSample {
    SystemFdSample {
        scope: TELEMETRY_SCOPE_SYSTEM.to_owned(),
        file_nr_allocated: MetricValue::known(10),
        file_nr_max: MetricValue::known(0),
        file_max: MetricValue::known(100),
        pressure_percent: MetricValue::known(10.0),
    }
}

fn snapshot_provider(snapshot: TelemetrySnapshot) -> SnapshotProvider {
    Arc::new(move || {
        let snapshot = snapshot.clone();
        Box::pin(async move { Ok(snapshot) })
    })
}

fn config(provider: SnapshotProvider) -> ServerConfig {
    ServerConfig::new("test-daemon", Arc::new(runtime)).with_snapshot_provider(provider)
}

async fn spawn_snapshot_server(
    provider: SnapshotProvider,
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
    let server = tokio::spawn(serve(listener, config(provider), shutdown_rx));
    (directory, path, shutdown_tx, server)
}

async fn request_snapshot_from(
    snapshot: TelemetrySnapshot,
) -> Result<TelemetrySnapshot, ClientError> {
    let (_directory, path, shutdown_tx, server) =
        spawn_snapshot_server(snapshot_provider(snapshot)).await;
    let result = request_telemetry_snapshot(&path, RequestEnvelope::telemetry_snapshot()).await;
    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
    result
}

#[tokio::test]
async fn snapshot_roundtrip_reassembles_current_schema_application_aggregates() {
    let expected = snapshot(65, 16);
    let actual = request_snapshot_from(expected.clone())
        .await
        .expect("snapshot");
    assert_eq!(actual, expected);
    assert_eq!(actual.applications.len(), 65);
}

#[tokio::test]
async fn zero_record_snapshot_uses_start_end_without_empty_chunks() {
    let expected = snapshot(0, 0);
    let actual = request_snapshot_from(expected.clone())
        .await
        .expect("empty snapshot");
    assert_eq!(actual, expected);
    assert!(actual.applications.is_empty());
}

#[tokio::test]
async fn client_rejects_invalid_start_declarations_before_collecting_records() {
    for case in 0..7 {
        let directory = tempdir().expect("socket directory");
        let path = directory.path().join(format!("appd-start-{case}.sock"));
        let listener = UnixListener::bind(&path).expect("listener");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut budget = WireBudget::new(1, 65_540);
            let request: RequestEnvelope = read_json(&mut stream, deadline, &mut budget)
                .await
                .expect("request");
            let mut declaration = start(1, 1);
            match case {
                0 => declaration.schema_version += 1,
                1 => declaration.total_applications = 1_025,
                2 => declaration.total_records = 4_097,
                3 => declaration.total_records = 2,
                4 => declaration.data_frame_count = 129,
                5 => declaration.data_frame_count = u32::MAX,
                _ => {
                    declaration.total_applications = 0;
                    declaration.total_records = 0;
                }
            }
            write_json(
                &mut stream,
                &envelope(
                    request.request_id,
                    0,
                    Some(Uuid::new_v4()),
                    ResponseBody::SnapshotStart(Box::new(declaration)),
                ),
                deadline,
            )
            .await
            .expect("invalid start");
        });

        let error = request_telemetry_snapshot(&path, RequestEnvelope::telemetry_snapshot())
            .await
            .expect_err("invalid Start declaration");
        assert_eq!(error.reason_code(), "snapshot_start_invalid");
        server.await.expect("join");
    }
}

#[tokio::test]
async fn server_rejects_schema_count_record_and_frame_limits_before_start() {
    let mut wrong_schema = snapshot(0, 0);
    wrong_schema.schema_version = TELEMETRY_SCHEMA_VERSION + 1;
    assert_daemon_code(
        request_snapshot_from(wrong_schema).await,
        "snapshot_schema_unsupported",
    );

    assert_daemon_code(
        request_snapshot_from(snapshot(1_025, 1)).await,
        "snapshot_application_limit",
    );
    assert_daemon_code(
        request_snapshot_from(snapshot(1, 65_536)).await,
        "snapshot_record_oversize",
    );

    request_snapshot_from(snapshot(128, 40_000))
        .await
        .expect("130 response frames accepted");
    assert_daemon_code(
        request_snapshot_from(snapshot(129, 40_000)).await,
        "snapshot_frame_limit",
    );
}

fn assert_daemon_code(result: Result<TelemetrySnapshot, ClientError>, expected: &str) {
    match result.expect_err("daemon error") {
        ClientError::Daemon(DaemonError { code, .. }) => assert_eq!(code, expected),
        other => panic!("expected daemon error {expected}, got {other:?}"),
    }
}

fn start(total: u32, frames: u32) -> SnapshotStart {
    SnapshotStart {
        schema_version: TELEMETRY_SCHEMA_VERSION,
        captured_at_unix_ms: Some(1),
        sample_interval_ms: Some(1_000),
        logical_cpu_count: Some(4),
        freshness: TelemetryFreshness::Fresh,
        status: TelemetryStatus::Complete,
        reason: "complete".to_owned(),
        retryable: false,
        scope: "same_euid".to_owned(),
        last_success_at_unix_ms: Some(1),
        system_fd: system_fd(),
        total_applications: total,
        total_records: total,
        data_frame_count: frames,
    }
}

fn end(total: u32, frames: u32) -> SnapshotEnd {
    SnapshotEnd {
        schema_version: TELEMETRY_SCHEMA_VERSION,
        freshness: TelemetryFreshness::Fresh,
        status: TelemetryStatus::Complete,
        reason: "complete".to_owned(),
        retryable: false,
        scope: "same_euid".to_owned(),
        last_success_at_unix_ms: Some(1),
        total_applications: total,
        total_records: total,
        data_frame_count: frames,
        permission_denied_counts: Vec::new(),
        issues: Vec::new(),
    }
}

fn envelope(
    request_id: Uuid,
    sequence: u32,
    snapshot_id: Option<Uuid>,
    body: ResponseBody,
) -> ResponseEnvelope {
    ResponseEnvelope {
        protocol_version: WIRE_PROTOCOL_VERSION,
        request_id,
        sequence,
        snapshot_id,
        body,
    }
}

#[tokio::test]
async fn client_rejects_sequence_identity_empty_chunk_and_second_start() {
    for case in 0..6 {
        let directory = tempdir().expect("socket directory");
        let path = directory.path().join(format!("appd-{case}.sock"));
        let listener = UnixListener::bind(&path).expect("listener");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut budget = WireBudget::new(1, 65_540);
            let request: RequestEnvelope = read_json(&mut stream, deadline, &mut budget)
                .await
                .expect("request");
            let snapshot_id = Uuid::new_v4();
            let first = envelope(
                request.request_id,
                0,
                Some(snapshot_id),
                ResponseBody::SnapshotStart(Box::new(start(1, 1))),
            );
            write_json(&mut stream, &first, deadline)
                .await
                .expect("start");
            let response = match case {
                0 => envelope(
                    request.request_id,
                    2,
                    Some(snapshot_id),
                    ResponseBody::ApplicationChunk(ApplicationChunk {
                        records: vec![application(0, 1)],
                    }),
                ),
                1 => envelope(
                    request.request_id,
                    1,
                    Some(Uuid::new_v4()),
                    ResponseBody::ApplicationChunk(ApplicationChunk {
                        records: vec![application(0, 1)],
                    }),
                ),
                2 => envelope(
                    Uuid::new_v4(),
                    1,
                    Some(snapshot_id),
                    ResponseBody::ApplicationChunk(ApplicationChunk {
                        records: vec![application(0, 1)],
                    }),
                ),
                3 => envelope(
                    request.request_id,
                    1,
                    Some(snapshot_id),
                    ResponseBody::ApplicationChunk(ApplicationChunk {
                        records: Vec::new(),
                    }),
                ),
                4 => envelope(
                    request.request_id,
                    1,
                    Some(snapshot_id),
                    ResponseBody::SnapshotStart(Box::new(start(1, 1))),
                ),
                _ => envelope(
                    request.request_id,
                    1,
                    Some(snapshot_id),
                    ResponseBody::ApplicationChunk(ApplicationChunk {
                        records: (0..33).map(|index| application(index, 1)).collect(),
                    }),
                ),
            };
            write_json(&mut stream, &response, deadline)
                .await
                .expect("response");
        });
        let error = request_telemetry_snapshot(&path, RequestEnvelope::telemetry_snapshot())
            .await
            .expect_err("protocol violation");
        let expected = match case {
            0 => "appd_sequence_invalid",
            1 => "snapshot_identity_mismatch",
            2 => "appd_request_id_mismatch",
            3 => "snapshot_chunk_invalid",
            4 => "appd_response_body_invalid",
            _ => "snapshot_chunk_invalid",
        };
        assert_eq!(error.reason_code(), expected);
        server.await.expect("join");
    }
}

#[tokio::test]
async fn client_rejects_duplicate_gap_and_out_of_order_sequences() {
    for invalid_sequence in [1, 3, 0] {
        let directory = tempdir().expect("socket directory");
        let path = directory
            .path()
            .join(format!("appd-sequence-{invalid_sequence}.sock"));
        let listener = UnixListener::bind(&path).expect("listener");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut budget = WireBudget::new(1, 65_540);
            let request: RequestEnvelope = read_json(&mut stream, deadline, &mut budget)
                .await
                .expect("request");
            let snapshot_id = Uuid::new_v4();
            for response in [
                envelope(
                    request.request_id,
                    0,
                    Some(snapshot_id),
                    ResponseBody::SnapshotStart(Box::new(start(2, 2))),
                ),
                envelope(
                    request.request_id,
                    1,
                    Some(snapshot_id),
                    ResponseBody::ApplicationChunk(ApplicationChunk {
                        records: vec![application(0, 1)],
                    }),
                ),
                envelope(
                    request.request_id,
                    invalid_sequence,
                    Some(snapshot_id),
                    ResponseBody::ApplicationChunk(ApplicationChunk {
                        records: vec![application(1, 1)],
                    }),
                ),
            ] {
                write_json(&mut stream, &response, deadline)
                    .await
                    .expect("fixture response");
            }
        });

        let error = request_telemetry_snapshot(&path, RequestEnvelope::telemetry_snapshot())
            .await
            .expect_err("invalid sequence");
        assert_eq!(error.reason_code(), "appd_sequence_invalid");
        server.await.expect("join");
    }
}

#[tokio::test]
async fn client_rejects_record_overrun_end_mismatch_missing_terminal_and_trailing_data() {
    for case in 0..4 {
        let directory = tempdir().expect("socket directory");
        let path = directory.path().join(format!("appd-end-{case}.sock"));
        let listener = UnixListener::bind(&path).expect("listener");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut budget = WireBudget::new(1, 65_540);
            let request: RequestEnvelope = read_json(&mut stream, deadline, &mut budget)
                .await
                .expect("request");
            let snapshot_id = Uuid::new_v4();
            let declared_total = if case == 0 { 1 } else { 0 };
            let declared_frames = if case == 0 { 1 } else { 0 };
            write_json(
                &mut stream,
                &envelope(
                    request.request_id,
                    0,
                    Some(snapshot_id),
                    ResponseBody::SnapshotStart(Box::new(start(declared_total, declared_frames))),
                ),
                deadline,
            )
            .await
            .expect("start");
            match case {
                0 => {
                    write_json(
                        &mut stream,
                        &envelope(
                            request.request_id,
                            1,
                            Some(snapshot_id),
                            ResponseBody::ApplicationChunk(ApplicationChunk {
                                records: vec![application(0, 1), application(1, 1)],
                            }),
                        ),
                        deadline,
                    )
                    .await
                    .expect("overrun");
                }
                1 => {
                    let mut invalid_end = end(0, 0);
                    invalid_end.schema_version += 1;
                    write_json(
                        &mut stream,
                        &envelope(
                            request.request_id,
                            1,
                            Some(snapshot_id),
                            ResponseBody::SnapshotEnd(invalid_end),
                        ),
                        deadline,
                    )
                    .await
                    .expect("end");
                }
                2 => {}
                _ => {
                    write_json(
                        &mut stream,
                        &envelope(
                            request.request_id,
                            1,
                            Some(snapshot_id),
                            ResponseBody::SnapshotEnd(end(0, 0)),
                        ),
                        deadline,
                    )
                    .await
                    .expect("end");
                    stream.write_all(b"x").await.expect("trailing");
                }
            }
            stream.shutdown().await.expect("shutdown");
        });
        let error = request_telemetry_snapshot(&path, RequestEnvelope::telemetry_snapshot())
            .await
            .expect_err("protocol violation");
        let expected = match case {
            0 => "snapshot_chunk_invalid",
            1 => "snapshot_end_invalid",
            2 => "snapshot_terminal_missing",
            _ => "appd_trailing_response_data",
        };
        assert_eq!(error.reason_code(), expected);
        server.await.expect("join");
    }
}

#[tokio::test]
async fn midstream_error_requires_next_sequence_and_matching_snapshot_identity() {
    for case in 0..4 {
        let directory = tempdir().expect("socket directory");
        let path = directory.path().join(format!("appd-error-{case}.sock"));
        let listener = UnixListener::bind(&path).expect("listener");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut budget = WireBudget::new(1, 65_540);
            let request: RequestEnvelope = read_json(&mut stream, deadline, &mut budget)
                .await
                .expect("request");
            let snapshot_id = Uuid::new_v4();
            write_json(
                &mut stream,
                &envelope(
                    request.request_id,
                    0,
                    Some(snapshot_id),
                    ResponseBody::SnapshotStart(Box::new(start(2, 2))),
                ),
                deadline,
            )
            .await
            .expect("start");
            write_json(
                &mut stream,
                &envelope(
                    request.request_id,
                    1,
                    Some(snapshot_id),
                    ResponseBody::ApplicationChunk(ApplicationChunk {
                        records: vec![application(0, 1)],
                    }),
                ),
                deadline,
            )
            .await
            .expect("chunk");
            let response = envelope(
                if case == 2 {
                    Uuid::new_v4()
                } else {
                    request.request_id
                },
                if case == 1 { 3 } else { 2 },
                match case {
                    0 => None,
                    3 => Some(snapshot_id),
                    _ => Some(Uuid::new_v4()),
                },
                ResponseBody::Error(DaemonError::new("fixture_error", "fixture_error", true)),
            );
            write_json(&mut stream, &response, deadline)
                .await
                .expect("error");
            stream.shutdown().await.expect("shutdown");
        });
        let error = request_telemetry_snapshot(&path, RequestEnvelope::telemetry_snapshot())
            .await
            .expect_err("terminal error");
        match case {
            0 => assert_eq!(error.reason_code(), "snapshot_identity_mismatch"),
            1 => assert_eq!(error.reason_code(), "appd_sequence_invalid"),
            2 => assert_eq!(error.reason_code(), "appd_request_id_mismatch"),
            _ => match error {
                ClientError::Daemon(error) => assert_eq!(error.code, "fixture_error"),
                other => panic!("expected daemon error, got {other:?}"),
            },
        }
        server.await.expect("join");
    }
}

#[tokio::test]
async fn four_snapshot_streams_preserve_health_and_fifth_gets_typed_busy_error() {
    let barrier = Arc::new(Barrier::new(5));
    let release = Arc::new(Semaphore::new(0));
    let held_snapshot = snapshot(1, 1);
    let provider: SnapshotProvider = Arc::new({
        let barrier = barrier.clone();
        let release = release.clone();
        move || {
            let barrier = barrier.clone();
            let release = release.clone();
            let snapshot = held_snapshot.clone();
            Box::pin(async move {
                barrier.wait().await;
                release
                    .acquire_owned()
                    .await
                    .expect("release semaphore")
                    .forget();
                Ok(snapshot)
            })
        }
    });
    let (_directory, path, shutdown_tx, server) = spawn_snapshot_server(provider).await;
    let mut requests = Vec::new();
    for _ in 0..4 {
        let path = path.clone();
        requests.push(tokio::spawn(async move {
            request_telemetry_snapshot(&path, RequestEnvelope::telemetry_snapshot()).await
        }));
    }
    barrier.wait().await;

    let health = localdesk_ipc::request_health(&path, RequestEnvelope::appd_health("test-client"))
        .await
        .expect("health remains available");
    assert_eq!(health.health, localdesk_domain::HealthState::Healthy);

    let fifth = request_telemetry_snapshot(&path, RequestEnvelope::telemetry_snapshot())
        .await
        .expect_err("fifth snapshot is busy");
    match fifth {
        ClientError::Daemon(error) => {
            assert_eq!(error.code, "snapshot_capacity_exceeded");
            assert!(error.retryable);
        }
        other => panic!("expected typed busy error, got {other:?}"),
    }

    release.add_permits(4);
    for request in requests {
        request.await.expect("join").expect("snapshot");
    }
    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}
