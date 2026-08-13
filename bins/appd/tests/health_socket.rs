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
#[path = "../src/socket.rs"]
mod socket;
#[allow(dead_code)]
#[path = "../src/usage.rs"]
mod usage;

use localdesk_domain::{
    APPD_HEALTH_CAPABILITY, CapabilityAvailability, HealthState, TELEMETRY_SNAPSHOT_CAPABILITY,
};
use localdesk_ipc::{RequestEnvelope, request_health};
use localdesk_network::NetworkMonitor;
use localdesk_telemetry::TelemetryManager;
use network::NetworkSupervisor;
use remote::RemoteRuntime;
use std::os::unix::fs::PermissionsExt;
use tempfile::tempdir;
use tokio::sync::watch;
use usage::UsageHandle;

#[tokio::test]
async fn health_socket_has_private_runtime_and_dynamic_capabilities() {
    let runtime = tempdir().expect("temporary runtime directory");
    std::fs::set_permissions(runtime.path(), std::fs::Permissions::from_mode(0o700))
        .expect("private runtime directory");
    let bound = socket::bind_appd_socket(runtime.path())
        .await
        .expect("bind appd socket");
    let socket_path = bound.path.clone();
    let directory = socket_path.parent().expect("socket directory");
    let directory_mode = std::fs::metadata(directory)
        .expect("directory metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(directory_mode, 0o700);
    let socket_mode = std::fs::symlink_metadata(&socket_path)
        .expect("socket metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(socket_mode, 0o600);

    let manager = TelemetryManager::with_defaults();
    let network = NetworkSupervisor::new(NetworkMonitor::default());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let server = tokio::spawn(service::serve_appd(
        bound.listener,
        manager.handle(),
        network.handle(),
        UsageHandle::unavailable_for_test("usage_fixture_unavailable"),
        notes::NotesHandle::unavailable_for_test(),
        RemoteRuntime::unavailable_for_test("remote_fixture_unavailable"),
        shutdown_rx,
    ));
    let hello = RequestEnvelope::health(
        "test-client",
        vec![
            APPD_HEALTH_CAPABILITY.to_owned(),
            TELEMETRY_SNAPSHOT_CAPABILITY.to_owned(),
        ],
    );
    let response = request_health(&socket_path, hello)
        .await
        .expect("health response");

    assert_eq!(response.health, HealthState::Degraded);
    assert_eq!(response.capabilities[0].id, APPD_HEALTH_CAPABILITY);
    assert_eq!(
        response.capabilities[0].status,
        CapabilityAvailability::Healthy
    );
    assert_eq!(response.capabilities[1].id, TELEMETRY_SNAPSHOT_CAPABILITY);
    assert_eq!(
        response.capabilities[1].status,
        CapabilityAvailability::Unreachable
    );
    assert_eq!(response.capabilities[1].reason, "telemetry_unavailable");
    assert!(response.capabilities.iter().all(|capability| {
        capability.status != CapabilityAvailability::Healthy
            || capability.id == APPD_HEALTH_CAPABILITY
    }));

    shutdown_tx.send(true).expect("shutdown signal");
    server
        .await
        .expect("server task join")
        .expect("server stop");
    socket::remove_socket(&socket_path).expect("remove socket");
}

#[tokio::test]
async fn symlink_stale_path_is_rejected_without_removal() {
    let runtime = tempdir().expect("temporary runtime directory");
    std::fs::set_permissions(runtime.path(), std::fs::Permissions::from_mode(0o700))
        .expect("private runtime directory");
    let directory = runtime.path().join(socket::SOCKET_DIRECTORY);
    std::fs::create_dir(&directory).expect("socket directory");
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
        .expect("private socket directory");
    let target = runtime.path().join("target");
    std::fs::write(&target, b"do not remove").expect("target file");
    let socket_path = directory.join(socket::SOCKET_NAME);
    std::os::unix::fs::symlink(&target, &socket_path).expect("symlink socket path");

    let error = socket::bind_appd_socket(runtime.path())
        .await
        .expect_err("symlink must be rejected");
    assert!(error.to_string().contains("symlink"));
    assert!(socket_path.is_symlink());
    assert_eq!(
        std::fs::read(&target).expect("target remains"),
        b"do not remove"
    );
}
