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

use localdesk_domain::{
    CapabilityAvailability, NETWORK_PER_APP_CAPABILITY, NETWORK_SCHEMA_VERSION,
    NETWORK_SYSTEM_CAPABILITY,
};
use localdesk_ipc::{RequestEnvelope, request_health, request_network_snapshot};
use localdesk_network::{MAX_RAW_INTERFACES, NetworkMonitor};
use localdesk_telemetry::TelemetryManager;
use network::NetworkSupervisor;
use remote::RemoteRuntime;
use tempfile::tempdir;
use tokio::{
    net::UnixListener,
    sync::watch,
    time::{Duration, sleep},
};
use usage::UsageHandle;

#[tokio::test]
async fn real_rtnetlink_snapshot_is_exposed_without_fabricated_per_app_records() {
    let directory = tempdir().expect("socket directory");
    let path = directory.path().join("appd.sock");
    let listener = UnixListener::bind(&path).expect("listener");
    let telemetry = TelemetryManager::with_defaults();
    let network = NetworkSupervisor::new(NetworkMonitor::default());
    let network_handle = network.handle();
    let (network_shutdown_tx, network_shutdown_rx) = watch::channel(false);
    let (ipc_shutdown_tx, ipc_shutdown_rx) = watch::channel(false);
    let network_task = tokio::spawn(network.run(network_shutdown_rx));
    let server = tokio::spawn(service::serve_appd(
        listener,
        telemetry.handle(),
        network_handle,
        UsageHandle::unavailable_for_test("usage_fixture_unavailable"),
        notes::NotesHandle::unavailable_for_test(),
        RemoteRuntime::unavailable_for_test("remote_fixture_unavailable"),
        ipc_shutdown_rx,
    ));

    sleep(Duration::from_millis(100)).await;
    let snapshot = request_network_snapshot(&path, RequestEnvelope::network_snapshot())
        .await
        .expect("network snapshot");
    assert_eq!(snapshot.schema_version, NETWORK_SCHEMA_VERSION);
    assert!(snapshot.interfaces.len() <= MAX_RAW_INTERFACES);
    assert_eq!(snapshot.validate(), Ok(()));
    assert_eq!(
        snapshot.per_application.status,
        CapabilityAvailability::Unsupported
    );
    assert!(snapshot.applications.is_empty());

    let health = request_health(
        &path,
        RequestEnvelope::health(
            "test-client",
            vec![
                NETWORK_SYSTEM_CAPABILITY.to_owned(),
                NETWORK_PER_APP_CAPABILITY.to_owned(),
            ],
        ),
    )
    .await
    .expect("network health");
    assert_eq!(
        health.capabilities[0].status,
        snapshot.system_traffic.status
    );
    assert_eq!(
        health.capabilities[1].status,
        CapabilityAvailability::Unsupported
    );
    assert_eq!(
        health.capabilities[1].reason,
        snapshot.per_application.reason
    );

    network_shutdown_tx.send(true).expect("network shutdown");
    ipc_shutdown_tx.send(true).expect("IPC shutdown");
    network_task.await.expect("network join");
    server.await.expect("server join").expect("server stop");
}
