#[allow(dead_code)]
#[path = "../src/network.rs"]
mod network;
#[allow(dead_code)]
#[path = "../src/notes.rs"]
mod notes;
#[allow(dead_code)]
#[path = "../src/remote.rs"]
mod remote;
#[path = "../src/service.rs"]
mod service;
#[allow(dead_code)]
#[path = "../src/usage.rs"]
mod usage;
#[allow(dead_code)]
#[path = "../src/speedtest.rs"]
mod speedtest;

use speedtest::SpeedTestHandle;
use localdesk_domain::{
    CapabilityAvailability, USAGE_FOREGROUND_CAPABILITY, UsagePeriod, UsageSummaryQuery,
};
use localdesk_ipc::{
    ClientError, DaemonError, RequestEnvelope, request_health, request_usage_summary,
};
use localdesk_network::NetworkMonitor;
use localdesk_telemetry::TelemetryManager;
use network::NetworkSupervisor;
use remote::RemoteRuntime;
use tempfile::tempdir;
use tokio::{net::UnixListener, sync::watch};
use usage::UsageHandle;

#[tokio::test]
async fn unavailable_usage_worker_is_typed_in_health_and_summary_requests() {
    let directory = tempdir().expect("socket directory");
    let path = directory.path().join("appd.sock");
    let listener = UnixListener::bind(&path).expect("listener");
    let telemetry = TelemetryManager::with_defaults();
    let network = NetworkSupervisor::new(NetworkMonitor::default());
    let usage = UsageHandle::unavailable_for_test("usage_database_unavailable");
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let server = tokio::spawn(service::serve_appd(
        listener,
        telemetry.handle(),
        network.handle(),
        usage,
        notes::NotesHandle::unavailable_for_test(),
        RemoteRuntime::unavailable_for_test("remote_fixture_unavailable"),
        SpeedTestHandle::new(),
                shutdown_rx,
    ));

    let health = request_health(
        &path,
        RequestEnvelope::health("test-client", vec![USAGE_FOREGROUND_CAPABILITY.to_owned()]),
    )
    .await
    .expect("usage health");
    assert_eq!(
        health.capabilities[0].status,
        CapabilityAvailability::Unreachable
    );
    assert_eq!(health.capabilities[0].reason, "usage_database_unavailable");

    let result = request_usage_summary(
        &path,
        RequestEnvelope::usage_summary(UsageSummaryQuery {
            period: UsagePeriod::Daily,
            bucket_key: "2026-08-09".to_owned(),
        }),
    )
    .await;
    match result.expect_err("unavailable usage provider") {
        ClientError::Daemon(DaemonError { code, reason, .. }) => {
            assert_eq!(code, "usage_provider_unavailable");
            assert_eq!(reason, "usage_database_unavailable");
        }
        other => panic!("expected daemon error, got {other:?}"),
    }

    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}
