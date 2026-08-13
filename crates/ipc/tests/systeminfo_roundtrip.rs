use localdesk_domain::{CapabilityAvailability, CapabilityRuntime, CapabilityRuntimeState};
use localdesk_ipc::{
    DaemonError, SystemInfoProvider, SystemInfoProviderFuture, SystemInfoReport,
    RequestEnvelope, ServerConfig, ServerError, request_system_info, serve,
};
use localdesk_systeminfo::{
    SystemInfoEntry, SystemInfoGroup, SystemInfoSection, SYSTEM_INFO_SCHEMA_VERSION,
};
use std::sync::Arc;
use tempfile::tempdir;
use tokio::{net::UnixListener, sync::watch};

fn runtime() -> CapabilityRuntime {
    CapabilityRuntime::new(
        CapabilityRuntimeState::healthy("appd_online"),
        CapabilityRuntimeState::degraded("telemetry_warming_up"),
        CapabilityRuntimeState::healthy("network_available"),
        CapabilityRuntimeState::unsupported("per_app_unavailable"),
        CapabilityRuntimeState::degraded("usage_warming_up"),
    )
}

fn sample_report() -> SystemInfoReport {
    SystemInfoReport {
        schema_version: SYSTEM_INFO_SCHEMA_VERSION,
        captured_at_unix_ms: Some(1_784_000_000_000),
        tool_version: Some("fastfetch 2.67.0 (x86_64)".to_owned()),
        status: CapabilityAvailability::Healthy,
        reason: "fastfetch_ok".to_owned(),
        retryable: false,
        sections: vec![SystemInfoSection {
            id: "OS".to_owned(),
            groups: vec![SystemInfoGroup {
                title: None,
                entries: vec![
                    SystemInfoEntry::new("os_name", "CachyOS Linux"),
                    SystemInfoEntry::new("os_version", "rolling · cachyos"),
                ],
            }],
        }],
    }
}

fn config(provider: Option<SystemInfoProvider>) -> ServerConfig {
    let mut config = ServerConfig::new("test-daemon", Arc::new(runtime));
    if let Some(provider) = provider {
        config = config.with_system_info_provider(provider);
    }
    config
}

async fn spawn_server(provider: Option<SystemInfoProvider>) -> (
    tempfile::TempDir,
    std::path::PathBuf,
    watch::Sender<bool>,
    tokio::task::JoinHandle<Result<(), ServerError>>,
) {
    let directory = tempdir().expect("socket directory");
    let path = directory.path().join("appd.sock");
    let listener = UnixListener::bind(&path).expect("listener");
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let server = tokio::spawn(serve(listener, config(provider), shutdown_rx));
    (directory, path, shutdown_tx, server)
}

#[tokio::test]
async fn system_info_roundtrips_the_provider_report() {
    let provider: SystemInfoProvider = Arc::new(|| {
        let future: SystemInfoProviderFuture = Box::pin(async move { Ok(sample_report()) });
        future
    });
    let (_directory, path, shutdown_tx, server) = spawn_server(Some(provider)).await;

    let report = request_system_info(&path, RequestEnvelope::system_info())
        .await
        .expect("system info");
    assert_eq!(report.schema_version, SYSTEM_INFO_SCHEMA_VERSION);
    assert_eq!(report.status, CapabilityAvailability::Healthy);
    assert_eq!(report.reason, "fastfetch_ok");
    assert_eq!(report.tool_version.as_deref(), Some("fastfetch 2.67.0 (x86_64)"));
    assert_eq!(report.captured_at_unix_ms, Some(1_784_000_000_000));
    assert_eq!(report.sections.len(), 1);
    assert_eq!(report.sections[0].id, "OS");
    assert_eq!(report.sections[0].groups[0].entries[0].key, "os_name");
    assert_eq!(report.sections[0].groups[0].entries[0].value, "CachyOS Linux");

    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}

#[tokio::test]
async fn system_info_without_provider_returns_daemon_error() {
    let (_directory, path, shutdown_tx, server) = spawn_server(None).await;

    let error = request_system_info(&path, RequestEnvelope::system_info())
        .await
        .expect_err("provider missing");
    assert!(matches!(error, localdesk_ipc::ClientError::Daemon(DaemonError { .. })));
    if let localdesk_ipc::ClientError::Daemon(daemon) = error {
        assert_eq!(daemon.code, "system_info_provider_unavailable");
        assert!(daemon.retryable);
    }

    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}

#[tokio::test]
async fn system_info_rejects_schema_version_mismatch() {
    let provider: SystemInfoProvider = Arc::new(|| {
        let mut report = sample_report();
        report.schema_version = 999;
        let future: SystemInfoProviderFuture = Box::pin(async move { Ok(report) });
        future
    });
    let (_directory, path, shutdown_tx, server) = spawn_server(Some(provider)).await;

    let error = request_system_info(&path, RequestEnvelope::system_info())
        .await
        .expect_err("schema mismatch");
    assert!(matches!(error, localdesk_ipc::ClientError::Daemon(DaemonError { .. })));
    if let localdesk_ipc::ClientError::Daemon(daemon) = error {
        assert_eq!(daemon.code, "system_info_invalid");
    }

    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}
