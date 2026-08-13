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

use localdesk_domain::{HealthState, TelemetryFreshness, TelemetryStatus};
use localdesk_ipc::{RequestEnvelope, request_health, request_telemetry_snapshot};
use localdesk_network::NetworkMonitor;
use localdesk_telemetry::{TelemetryManager, TelemetryStoreConfig};
use network::NetworkSupervisor;
use remote::RemoteRuntime;
use tempfile::tempdir;
use tokio::{net::UnixListener, sync::watch};
use usage::UsageHandle;

#[tokio::test]
async fn collector_unavailable_keeps_health_response_and_reports_snapshot_reason() {
    let directory = tempdir().expect("socket directory");
    let path = directory.path().join("appd.sock");
    let listener = UnixListener::bind(&path).expect("listener");
    let manager = TelemetryManager::new(TelemetryStoreConfig {
        stale_after: std::time::Duration::from_millis(2_500),
        max_stale: std::time::Duration::from_secs(10),
    });
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let network = NetworkSupervisor::new(NetworkMonitor::default());
    let server = tokio::spawn(service::serve_appd(
        listener,
        manager.handle(),
        network.handle(),
        UsageHandle::unavailable_for_test("usage_fixture_unavailable"),
        notes::NotesHandle::unavailable_for_test(),
        RemoteRuntime::unavailable_for_test("remote_fixture_unavailable"),
        shutdown_rx,
    ));

    let health_request =
        RequestEnvelope::health("test-client", vec!["telemetry.snapshot.v1".to_owned()]);
    let health = request_health(&path, health_request).await.expect("health");
    assert_eq!(health.health, HealthState::Degraded);
    assert_eq!(health.capabilities[0].reason, "telemetry_unavailable");

    let snapshot = request_telemetry_snapshot(&path, RequestEnvelope::telemetry_snapshot())
        .await
        .expect("snapshot response");
    assert_eq!(snapshot.status, TelemetryStatus::Unavailable);
    assert_eq!(snapshot.freshness, TelemetryFreshness::Unknown);
    assert_eq!(snapshot.reason, "collector_unavailable");
    assert_eq!(snapshot.sample_interval_ms, None);
    assert_eq!(snapshot.logical_cpu_count, None);

    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}
