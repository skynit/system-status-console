use localdesk_domain::{
    CapabilityAvailability, CapabilityRuntime, CapabilityRuntimeState, NetworkApplicationTraffic,
    NetworkByteTotals, NetworkInterfaceKind, NetworkInterfaceSample, NetworkInterfaceTransition,
    NetworkRate, NetworkRateState, NetworkSnapshot, UsageApplicationDuration, UsagePeriod,
    UsageSummary, UsageSummaryQuery,
};
use localdesk_ipc::{
    ClientError, DaemonError, NetworkApplicationChunk, NetworkInterfaceChunk, NetworkSnapshotEnd,
    NetworkSnapshotProvider, NetworkSnapshotStart, RequestEnvelope, ResponseBody, ResponseEnvelope,
    ServerConfig, SnapshotProviderError, UsageApplicationChunk, UsageSummaryEnd,
    UsageSummaryProvider, UsageSummaryStart, WIRE_PROTOCOL_VERSION, WireBudget, read_json,
    request_network_snapshot, request_usage_summary, serve, write_json,
};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tempfile::tempdir;
use tokio::{io::AsyncWriteExt, net::UnixListener, sync::watch, time::Instant};
use uuid::Uuid;

fn runtime() -> CapabilityRuntime {
    CapabilityRuntime::new(
        CapabilityRuntimeState::healthy("appd_online"),
        CapabilityRuntimeState::healthy("telemetry_available"),
        CapabilityRuntimeState::healthy("network_available"),
        CapabilityRuntimeState::unsupported("per_app_unavailable"),
        CapabilityRuntimeState::healthy("usage_available"),
    )
}

fn daily_query() -> UsageSummaryQuery {
    UsageSummaryQuery {
        period: UsagePeriod::Daily,
        bucket_key: "2026-08-09".to_owned(),
    }
}

fn network_snapshot(interface_count: usize, application_count: usize) -> NetworkSnapshot {
    let mut snapshot = NetworkSnapshot::unavailable("fixture_partial");
    snapshot.interfaces = (0..interface_count)
        .map(|index| NetworkInterfaceSample {
            index: u32::try_from(index + 1).expect("interface index"),
            name: format!("test{index}"),
            kind: NetworkInterfaceKind::Virtual,
            kernel_kind: Some("fixture".to_owned()),
            is_up: true,
            carrier_up: true,
            counters: None,
            rate: NetworkRate {
                rx_bytes_per_second: None,
                tx_bytes_per_second: None,
                state: NetworkRateState::CountersUnavailable,
                reason: "fixture_counters_unavailable".to_owned(),
            },
            transition: NetworkInterfaceTransition::CountersUnavailable,
        })
        .collect();
    snapshot.applications = (0..application_count)
        .map(|index| NetworkApplicationTraffic {
            application_key: format!("app-{index}"),
            rx_bytes: index as u64,
            tx_bytes: index as u64,
            rx_share_percent: None,
            tx_share_percent: None,
        })
        .collect();
    snapshot.per_application.status = if application_count == 0 {
        CapabilityAvailability::Unsupported
    } else {
        CapabilityAvailability::Degraded
    };
    snapshot.per_application.reason = "fixture_partial".to_owned();
    snapshot.coverage.reported_interfaces = u32::try_from(interface_count).expect("count");
    snapshot.coverage.interfaces_with_counters = 0;
    snapshot
}

fn usage_summary(application_count: usize) -> UsageSummary {
    let query = daily_query();
    let mut summary = UsageSummary::unavailable(query.clone(), "fixture_partial", true);
    summary.applications = (0..application_count)
        .map(|index| UsageApplicationDuration {
            app_id: format!("app-{index}"),
            bucket_key: query.bucket_key.clone(),
            timezone_id: "Asia/Shanghai".to_owned(),
            utc_offset_seconds: 28_800,
            duration_ns: index as u64,
            last_wall_utc_ms: 1,
        })
        .collect();
    summary
}

fn network_provider(snapshot: NetworkSnapshot) -> NetworkSnapshotProvider {
    Arc::new(move || {
        let snapshot = snapshot.clone();
        Box::pin(async move { Ok(snapshot) })
    })
}

fn usage_provider(summary: UsageSummary) -> UsageSummaryProvider {
    Arc::new(move |_| {
        let summary = summary.clone();
        Box::pin(async move { Ok(summary) })
    })
}

async fn spawn_server(
    config: ServerConfig,
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
    let server = tokio::spawn(serve(listener, config, shutdown_rx));
    (directory, path, shutdown_tx, server)
}

#[tokio::test]
async fn network_and_usage_multichunk_roundtrip_preserves_exact_records() {
    let network = network_snapshot(256, 1_024);
    let usage = usage_summary(1_024);
    let config = ServerConfig::new("fixture", Arc::new(runtime))
        .with_network_snapshot_provider(network_provider(network.clone()))
        .with_usage_summary_provider(usage_provider(usage.clone()));
    let (_directory, path, shutdown_tx, server) = spawn_server(config).await;

    let actual_network = request_network_snapshot(&path, RequestEnvelope::network_snapshot())
        .await
        .expect("network snapshot");
    let actual_usage = request_usage_summary(&path, RequestEnvelope::usage_summary(daily_query()))
        .await
        .expect("usage summary");
    assert_eq!(actual_network, network);
    assert_eq!(actual_network.interfaces.len(), 256);
    assert_eq!(actual_network.applications.len(), 1_024);
    assert_eq!(actual_usage, usage);
    assert_eq!(actual_usage.applications.len(), 1_024);

    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}

#[tokio::test]
async fn missing_network_and_usage_providers_are_typed_daemon_errors() {
    let config = ServerConfig::new("fixture", Arc::new(runtime));
    let (_directory, path, shutdown_tx, server) = spawn_server(config).await;

    assert_daemon_code(
        request_network_snapshot(&path, RequestEnvelope::network_snapshot()).await,
        "network_provider_unavailable",
    );
    assert_daemon_code(
        request_usage_summary(&path, RequestEnvelope::usage_summary(daily_query())).await,
        "usage_provider_unavailable",
    );

    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}

#[tokio::test]
async fn provider_errors_remain_typed_and_are_sent_before_any_start() {
    let network_provider: NetworkSnapshotProvider = Arc::new(|| {
        Box::pin(async {
            Err(SnapshotProviderError::new(
                "rtnetlink_unavailable",
                "rtnetlink_unavailable",
                true,
            ))
        })
    });
    let usage_provider: UsageSummaryProvider = Arc::new(|_| {
        Box::pin(async {
            Err(SnapshotProviderError::new(
                "usage_store_unavailable",
                "usage_store_unavailable",
                true,
            ))
        })
    });
    let config = ServerConfig::new("fixture", Arc::new(runtime))
        .with_network_snapshot_provider(network_provider)
        .with_usage_summary_provider(usage_provider);
    let (_directory, path, shutdown_tx, server) = spawn_server(config).await;

    assert_daemon_code(
        request_network_snapshot(&path, RequestEnvelope::network_snapshot()).await,
        "rtnetlink_unavailable",
    );
    assert_daemon_code(
        request_usage_summary(&path, RequestEnvelope::usage_summary(daily_query())).await,
        "usage_store_unavailable",
    );

    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}

#[tokio::test]
async fn invalid_snapshot_identity_is_a_terminal_daemon_error_not_a_partial_stream() {
    let mut network = network_snapshot(1, 1);
    network.snapshot_id = Uuid::nil();
    let mut usage = usage_summary(1);
    usage.snapshot_id = Uuid::nil();
    let config = ServerConfig::new("fixture", Arc::new(runtime))
        .with_network_snapshot_provider(network_provider(network))
        .with_usage_summary_provider(usage_provider(usage));
    let (_directory, path, shutdown_tx, server) = spawn_server(config).await;

    assert_daemon_code(
        request_network_snapshot(&path, RequestEnvelope::network_snapshot()).await,
        "snapshot_identity_invalid",
    );
    assert_daemon_code(
        request_usage_summary(&path, RequestEnvelope::usage_summary(daily_query())).await,
        "snapshot_identity_invalid",
    );

    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}

fn envelope(
    request_id: Uuid,
    sequence: u32,
    snapshot_id: Uuid,
    body: ResponseBody,
) -> ResponseEnvelope {
    ResponseEnvelope {
        protocol_version: WIRE_PROTOCOL_VERSION,
        request_id,
        sequence,
        snapshot_id: Some(snapshot_id),
        body,
    }
}

fn network_start(
    snapshot: &NetworkSnapshot,
    interfaces: u32,
    applications: u32,
    frames: u32,
) -> NetworkSnapshotStart {
    NetworkSnapshotStart {
        schema_version: snapshot.schema_version,
        captured_at_unix_ms: snapshot.captured_at_unix_ms,
        observed_boottime_ms: snapshot.observed_boottime_ms,
        sample_interval_ms: snapshot.sample_interval_ms,
        last_success_at_unix_ms: snapshot.last_success_at_unix_ms,
        freshness: snapshot.freshness,
        retryable: snapshot.retryable,
        system_traffic: snapshot.system_traffic.clone(),
        per_application: snapshot.per_application.clone(),
        coverage: snapshot.coverage.clone(),
        totals: snapshot.totals.clone(),
        aggregate_rate: snapshot.aggregate_rate.clone(),
        total_interfaces: interfaces,
        total_applications: applications,
        total_records: interfaces + applications,
        data_frame_count: frames,
    }
}

fn network_end(
    snapshot: &NetworkSnapshot,
    interfaces: u32,
    applications: u32,
    frames: u32,
) -> NetworkSnapshotEnd {
    NetworkSnapshotEnd {
        schema_version: snapshot.schema_version,
        total_interfaces: interfaces,
        total_applications: applications,
        total_records: interfaces + applications,
        data_frame_count: frames,
    }
}

fn usage_start(summary: &UsageSummary, applications: u32, frames: u32) -> UsageSummaryStart {
    UsageSummaryStart {
        schema_version: summary.schema_version,
        captured_at_unix_ms: summary.captured_at_unix_ms,
        query: summary.query.clone(),
        status: summary.status,
        reason: summary.reason.clone(),
        retryable: summary.retryable,
        coverage: summary.coverage.clone(),
        total_applications: applications,
        total_records: applications,
        data_frame_count: frames,
    }
}

fn usage_end(summary: &UsageSummary, applications: u32, frames: u32) -> UsageSummaryEnd {
    UsageSummaryEnd {
        schema_version: summary.schema_version,
        query: summary.query.clone(),
        status: summary.status,
        reason: summary.reason.clone(),
        retryable: summary.retryable,
        total_applications: applications,
        total_records: applications,
        data_frame_count: frames,
    }
}

#[tokio::test]
async fn network_client_rejects_sequence_phase_and_end_count_violations() {
    for case in 0..3 {
        let directory = tempdir().expect("socket directory");
        let path = directory
            .path()
            .join(format!("network-invalid-{case}.sock"));
        let listener = UnixListener::bind(&path).expect("listener");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut budget = WireBudget::new(1, 65_540);
            let request: RequestEnvelope = read_json(&mut stream, deadline, &mut budget)
                .await
                .expect("request");
            let snapshot = network_snapshot(1, usize::from(case == 1));
            let snapshot_id = snapshot.snapshot_id;
            let frames = if case == 1 { 2 } else { 1 };
            write_json(
                &mut stream,
                &envelope(
                    request.request_id,
                    0,
                    snapshot_id,
                    ResponseBody::NetworkSnapshotStart(Box::new(network_start(
                        &snapshot,
                        1,
                        u32::from(case == 1),
                        frames,
                    ))),
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
                            2,
                            snapshot_id,
                            ResponseBody::NetworkInterfaceChunk(NetworkInterfaceChunk {
                                records: snapshot.interfaces.clone(),
                            }),
                        ),
                        deadline,
                    )
                    .await
                    .expect("invalid sequence");
                }
                1 => {
                    for response in [
                        envelope(
                            request.request_id,
                            1,
                            snapshot_id,
                            ResponseBody::NetworkApplicationChunk(NetworkApplicationChunk {
                                records: snapshot.applications.clone(),
                            }),
                        ),
                        envelope(
                            request.request_id,
                            2,
                            snapshot_id,
                            ResponseBody::NetworkInterfaceChunk(NetworkInterfaceChunk {
                                records: snapshot.interfaces.clone(),
                            }),
                        ),
                    ] {
                        write_json(&mut stream, &response, deadline)
                            .await
                            .expect("out of phase chunk");
                    }
                }
                _ => {
                    write_json(
                        &mut stream,
                        &envelope(
                            request.request_id,
                            1,
                            snapshot_id,
                            ResponseBody::NetworkInterfaceChunk(NetworkInterfaceChunk {
                                records: snapshot.interfaces.clone(),
                            }),
                        ),
                        deadline,
                    )
                    .await
                    .expect("chunk");
                    write_json(
                        &mut stream,
                        &envelope(
                            request.request_id,
                            2,
                            snapshot_id,
                            ResponseBody::NetworkSnapshotEnd(network_end(&snapshot, 2, 0, 1)),
                        ),
                        deadline,
                    )
                    .await
                    .expect("invalid end");
                }
            }
        });

        let error = request_network_snapshot(&path, RequestEnvelope::network_snapshot())
            .await
            .expect_err("network protocol violation");
        let expected = match case {
            0 => "appd_sequence_invalid",
            1 => "snapshot_chunk_invalid",
            _ => "snapshot_end_invalid",
        };
        assert_eq!(error.reason_code(), expected);
        server.await.expect("join");
    }
}

#[tokio::test]
async fn usage_client_rejects_identity_chunk_limit_missing_terminal_and_trailing_data() {
    for case in 0..4 {
        let directory = tempdir().expect("socket directory");
        let path = directory.path().join(format!("usage-invalid-{case}.sock"));
        let listener = UnixListener::bind(&path).expect("listener");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut budget = WireBudget::new(1, 65_540);
            let request: RequestEnvelope = read_json(&mut stream, deadline, &mut budget)
                .await
                .expect("request");
            let count = if case <= 1 { 33 } else { 0 };
            let summary = usage_summary(count);
            let snapshot_id = summary.snapshot_id;
            let frames = u32::from(count > 0);
            write_json(
                &mut stream,
                &envelope(
                    request.request_id,
                    0,
                    snapshot_id,
                    ResponseBody::UsageSummaryStart(Box::new(usage_start(
                        &summary,
                        count as u32,
                        frames,
                    ))),
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
                            Uuid::new_v4(),
                            ResponseBody::UsageApplicationChunk(UsageApplicationChunk {
                                records: summary.applications,
                            }),
                        ),
                        deadline,
                    )
                    .await
                    .expect("wrong identity");
                }
                1 => {
                    write_json(
                        &mut stream,
                        &envelope(
                            request.request_id,
                            1,
                            snapshot_id,
                            ResponseBody::UsageApplicationChunk(UsageApplicationChunk {
                                records: summary.applications,
                            }),
                        ),
                        deadline,
                    )
                    .await
                    .expect("oversize chunk");
                }
                2 => {}
                _ => {
                    write_json(
                        &mut stream,
                        &envelope(
                            request.request_id,
                            1,
                            snapshot_id,
                            ResponseBody::UsageSummaryEnd(usage_end(&summary, 0, 0)),
                        ),
                        deadline,
                    )
                    .await
                    .expect("end");
                    stream.write_all(b"x").await.expect("trailing byte");
                }
            }
            stream.shutdown().await.expect("shutdown");
        });

        let error = request_usage_summary(&path, RequestEnvelope::usage_summary(daily_query()))
            .await
            .expect_err("usage protocol violation");
        let expected = match case {
            0 => "snapshot_identity_mismatch",
            1 => "snapshot_chunk_invalid",
            2 => "snapshot_terminal_missing",
            _ => "appd_trailing_response_data",
        };
        assert_eq!(error.reason_code(), expected);
        server.await.expect("join");
    }
}

fn assert_daemon_code<T: std::fmt::Debug>(result: Result<T, ClientError>, expected: &str) {
    match result.expect_err("daemon error") {
        ClientError::Daemon(DaemonError { code, .. }) => assert_eq!(code, expected),
        other => panic!("expected daemon error {expected}, got {other:?}"),
    }
}

#[test]
fn fixture_shapes_are_valid() {
    assert_eq!(network_snapshot(256, 1_024).validate(), Ok(()));
    assert_eq!(usage_summary(1_024).validate(), Ok(()));
    assert_eq!(NetworkByteTotals::default().rx_bytes, 0);
}
