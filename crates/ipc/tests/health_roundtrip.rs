use localdesk_domain::{
    APPD_HEALTH_CAPABILITY, Capability, CapabilityAvailability, CapabilityRuntime,
    CapabilityRuntimeState, HealthState, TELEMETRY_SNAPSHOT_CAPABILITY,
};
use localdesk_ipc::{
    ClientError, DaemonError, HEALTH_TOTAL_DEADLINE, HealthReport, MAX_CONNECTIONS,
    MAX_SNAPSHOT_STREAMS, REMOTE_SESSION_TOTAL_DEADLINE, RequestEnvelope, ResponseBody,
    ResponseEnvelope, SHUTDOWN_GRACE, SNAPSHOT_TOTAL_DEADLINE, ServerConfig, WIRE_PROTOCOL_VERSION,
    WireBudget, read_json, request_health, serve, write_json,
};
use std::{sync::Arc, time::Duration};
use tempfile::tempdir;
use tokio::{
    io::AsyncWriteExt,
    net::{UnixListener, UnixStream},
    sync::watch,
    time::{Instant, sleep, timeout},
};

fn runtime() -> CapabilityRuntime {
    CapabilityRuntime::new(
        CapabilityRuntimeState::healthy("appd_online"),
        CapabilityRuntimeState::degraded("telemetry_warming_up"),
        CapabilityRuntimeState::healthy("network_available"),
        CapabilityRuntimeState::unsupported("per_app_unavailable"),
        CapabilityRuntimeState::degraded("usage_warming_up"),
    )
}

fn config() -> ServerConfig {
    ServerConfig::new("test-daemon", Arc::new(runtime))
}

async fn spawn_server() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    watch::Sender<bool>,
    tokio::task::JoinHandle<Result<(), localdesk_ipc::ServerError>>,
) {
    let directory = tempdir().expect("socket directory");
    let path = directory.path().join("appd.sock");
    let listener = UnixListener::bind(&path).expect("listener");
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let server = tokio::spawn(serve(listener, config(), shutdown_rx));
    (directory, path, shutdown_tx, server)
}

#[tokio::test]
async fn health_is_request_scoped_and_uses_runtime_capability_facts() {
    let (_directory, path, shutdown_tx, server) = spawn_server().await;
    let request = RequestEnvelope::health(
        "test-client",
        vec![
            APPD_HEALTH_CAPABILITY.to_owned(),
            TELEMETRY_SNAPSHOT_CAPABILITY.to_owned(),
        ],
    );

    let report = request_health(&path, request).await.expect("health");
    assert_eq!(report.daemon_version, "test-daemon");
    assert_eq!(report.health, HealthState::Degraded);
    assert_eq!(report.reason, "telemetry_warming_up");
    assert_eq!(report.capabilities.len(), 2);
    assert_eq!(report.capabilities[0].id, APPD_HEALTH_CAPABILITY);
    assert_eq!(
        report.capabilities[0].status,
        CapabilityAvailability::Healthy
    );
    assert_eq!(report.capabilities[1].id, TELEMETRY_SNAPSHOT_CAPABILITY);
    assert_eq!(
        report.capabilities[1].status,
        CapabilityAvailability::Degraded
    );

    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}

#[tokio::test]
async fn unknown_capability_is_typed_unsupported_without_catalog_expansion() {
    let (_directory, path, shutdown_tx, server) = spawn_server().await;
    let report = request_health(
        &path,
        RequestEnvelope::health("test-client", vec!["unknown.capability.v1".to_owned()]),
    )
    .await
    .expect("health");

    assert_eq!(report.health, HealthState::Degraded);
    assert_eq!(report.capabilities.len(), 1);
    assert_eq!(
        report.capabilities[0],
        Capability::new(
            "unknown.capability.v1",
            CapabilityAvailability::Unsupported,
            "unknown_capability",
        )
    );

    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}

#[tokio::test]
async fn empty_duplicate_and_over_32_capability_requests_are_typed_errors() {
    let (_directory, path, shutdown_tx, server) = spawn_server().await;
    let requests = [
        Vec::new(),
        vec![String::new()],
        vec![
            APPD_HEALTH_CAPABILITY.to_owned(),
            APPD_HEALTH_CAPABILITY.to_owned(),
        ],
        (0..33).map(|index| format!("capability-{index}")).collect(),
    ];

    for requested_capabilities in requests {
        let error = request_health(
            &path,
            RequestEnvelope::health("test-client", requested_capabilities),
        )
        .await
        .expect_err("invalid request");
        match error {
            ClientError::Daemon(DaemonError {
                code, retryable, ..
            }) => {
                assert_eq!(code, "invalid_request");
                assert!(!retryable);
            }
            other => panic!("expected daemon error, got {other:?}"),
        }
    }

    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}

#[tokio::test]
async fn transport_failure_is_not_reported_as_daemon_unreachable_health() {
    let directory = tempdir().expect("socket directory");
    let path = directory.path().join("missing.sock");
    let error = request_health(&path, RequestEnvelope::appd_health("test-client"))
        .await
        .expect_err("missing socket is a transport failure");
    assert!(matches!(&error, ClientError::Transport(_)));
    assert_eq!(error.reason_code(), "appd_connection_failed");
}

#[test]
fn deadline_and_capacity_constants_match_the_v12_contract() {
    assert_eq!(HEALTH_TOTAL_DEADLINE, Duration::from_secs(2));
    assert_eq!(SNAPSHOT_TOTAL_DEADLINE, Duration::from_secs(5));
    assert_eq!(REMOTE_SESSION_TOTAL_DEADLINE, Duration::from_secs(70));
    assert_eq!(SHUTDOWN_GRACE, Duration::from_secs(6));
    assert_eq!(MAX_CONNECTIONS, 32);
    assert_eq!(MAX_SNAPSHOT_STREAMS, 4);
}

#[tokio::test]
async fn slow_partial_request_does_not_block_bounded_shutdown() {
    let (_directory, path, shutdown_tx, server) = spawn_server().await;
    let mut stream = UnixStream::connect(&path).await.expect("connect");
    stream.write_all(&[0, 0]).await.expect("partial header");
    sleep(Duration::from_millis(20)).await;
    shutdown_tx.send(true).expect("shutdown");

    timeout(Duration::from_secs(3), server)
        .await
        .expect("shutdown remained bounded")
        .expect("join")
        .expect("serve");
}

#[tokio::test]
async fn legacy_protocol_version_twelve_is_rejected_without_a_compatibility_branch() {
    let (_directory, path, shutdown_tx, server) = spawn_server().await;
    let mut request = RequestEnvelope::appd_health("test-client");
    request.protocol_version = 12;

    let error = request_health(&path, request)
        .await
        .expect_err("legacy protocol rejected");
    match error {
        ClientError::Daemon(error) => {
            assert_eq!(error.code, "unsupported_protocol");
            assert_eq!(error.reason, "wire_protocol_version_must_be_13");
            assert!(!error.retryable);
        }
        other => panic!("expected daemon error, got {other:?}"),
    }

    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}

#[tokio::test]
async fn health_terminal_requires_exact_eof() {
    let directory = tempdir().expect("socket directory");
    let path = directory.path().join("appd.sock");
    let listener = UnixListener::bind(&path).expect("listener");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut budget = WireBudget::new(1, 65_540);
        let request: RequestEnvelope = read_json(&mut stream, deadline, &mut budget)
            .await
            .expect("request");
        let response = ResponseEnvelope {
            protocol_version: WIRE_PROTOCOL_VERSION,
            request_id: request.request_id,
            sequence: 0,
            snapshot_id: None,
            body: ResponseBody::HealthReport(HealthReport {
                daemon_version: "fixture".to_owned(),
                health: HealthState::Healthy,
                reason: "appd_online".to_owned(),
                capabilities: vec![Capability::new(
                    APPD_HEALTH_CAPABILITY,
                    CapabilityAvailability::Healthy,
                    "appd_online",
                )],
            }),
        };
        write_json(&mut stream, &response, deadline)
            .await
            .expect("response");
        stream.write_all(b"x").await.expect("trailing byte");
        stream.shutdown().await.expect("shutdown stream");
    });

    let error = request_health(&path, RequestEnvelope::appd_health("test-client"))
        .await
        .expect_err("trailing data");
    assert_eq!(error.reason_code(), "appd_trailing_response_data");
    server.await.expect("join");
}
