use localdesk_domain::{
    CapabilityAvailability, CapabilityRuntime, CapabilityRuntimeState, TelemetryFreshness,
    TelemetrySnapshot, TelemetryStatus,
};
use localdesk_ipc::{
    CapabilityProvider, NetworkSnapshotProvider, NetworkSnapshotProviderFuture, NotesProvider,
    NotesProviderFuture, RemoteCapabilitiesProvider, RemoteCapabilitiesProviderFuture,
    RemoteProfileProvider, RemoteProfileProviderFuture, RemoteSessionProvider,
    RemoteSessionProviderFuture, SecretCommandProvider, SecretCommandProviderFuture, ServerConfig,
    ServerError, SnapshotProvider, SnapshotProviderError, SnapshotProviderFuture,
    SpeedTestCancelProvider, SpeedTestCancelProviderFuture, SpeedTestDeepProvider,
    SpeedTestDeepProviderFuture, SpeedTestProvider, SpeedTestProviderFuture, SystemInfoProvider,
    SystemInfoProviderFuture, SystemInfoReport, TerminalProvider, TerminalProviderFuture,
    TransferLocalHandleProvider, TransferLocalHandleProviderFuture, TransferProvider,
    TransferProviderFuture, UsageSummaryProvider, UsageSummaryProviderFuture, serve,
};
use localdesk_systeminfo::{
    SYSTEM_INFO_SCHEMA_VERSION, SystemInfoCollector, SystemInfoStatus,
};
use localdesk_telemetry::TelemetryManagerHandle;
use std::sync::Arc;
use tokio::{net::UnixListener, sync::watch};

use crate::network::NetworkHandle;
use crate::notes::NotesHandle;
use crate::remote::RemoteRuntime;
use crate::speedtest::SpeedTestHandle;
use crate::usage::UsageHandle;

pub const DAEMON_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn server_config(
    telemetry_handle: TelemetryManagerHandle,
    network_handle: NetworkHandle,
    usage_handle: UsageHandle,
    notes_handle: NotesHandle,
    remote_runtime: RemoteRuntime,
    speedtest_handle: SpeedTestHandle,
) -> ServerConfig {
    remote_runtime.enable_transfer_provider();
    let capability_telemetry = telemetry_handle.clone();
    let capability_network = network_handle.clone();
    let capability_usage = usage_handle.clone();
    let capability_remote = remote_runtime.clone();
    let capability_notes = notes_handle.clone();
    let capability_speedtest = speedtest_handle.clone();
    let capability_provider: CapabilityProvider = Arc::new(move || {
        let telemetry = capability_state(capability_telemetry.snapshot());
        let (network_system, network_per_app) = capability_network.capability_states();
        let runtime = CapabilityRuntime::new(
            CapabilityRuntimeState::healthy("appd_online"),
            telemetry,
            network_system,
            network_per_app,
            capability_usage.capability_state(),
        );
        let (ssh, sftp, ftp, smb, transfers) = capability_remote.capability_states();
        let (speedtest, deeptest) = capability_speedtest.capability_states();
        runtime
            .with_remote(ssh, sftp, ftp, smb, transfers)
            .with_notes(capability_notes.capability_state())
            .with_speedtest(speedtest, deeptest)
    });

    let snapshot_provider: SnapshotProvider = Arc::new(move || {
        let snapshot = telemetry_handle.snapshot().map_err(|error| {
            SnapshotProviderError::new("telemetry_store_error", store_error_reason(&error), false)
        });
        let future: SnapshotProviderFuture = Box::pin(async move { snapshot });
        future
    });

    let network_snapshot_provider: NetworkSnapshotProvider = Arc::new(move || {
        let snapshot = network_handle.snapshot();
        let future: NetworkSnapshotProviderFuture = Box::pin(async move { Ok(snapshot) });
        future
    });

    let usage_summary_provider: UsageSummaryProvider = Arc::new(move |query| {
        let handle = usage_handle.clone();
        let future: UsageSummaryProviderFuture = Box::pin(async move {
            handle.summary(query).await.map_err(|error| {
                SnapshotProviderError::new(error.code, error.reason, error.retryable)
            })
        });
        future
    });

    let remote_profile_runtime = remote_runtime.clone();
    let secret_runtime = remote_runtime.clone();
    let remote_session_runtime = remote_runtime.clone();
    let terminal_runtime = remote_runtime.clone();
    let transfer_runtime = remote_runtime.clone();
    let transfer_local_handle_runtime = remote_runtime.clone();
    let remote_capabilities_provider: RemoteCapabilitiesProvider = Arc::new(move || {
        let catalog = remote_runtime.catalog();
        let future: RemoteCapabilitiesProviderFuture = Box::pin(async move { Ok(catalog) });
        future
    });
    let system_info_provider: SystemInfoProvider = Arc::new(|| {
        let future: SystemInfoProviderFuture = Box::pin(async move {
            let outcome = SystemInfoCollector::default().collect().await;
            Ok(SystemInfoReport {
                schema_version: SYSTEM_INFO_SCHEMA_VERSION,
                captured_at_unix_ms: outcome.captured_at_unix_ms,
                tool_version: outcome.tool_version,
                status: match outcome.status {
                    SystemInfoStatus::Healthy => CapabilityAvailability::Healthy,
                    SystemInfoStatus::Degraded => CapabilityAvailability::Degraded,
                    SystemInfoStatus::Unsupported => CapabilityAvailability::Unsupported,
                },
                reason: outcome.reason,
                retryable: outcome.retryable,
                sections: outcome.sections,
            })
        });
        future
    });
    let remote_profile_provider: RemoteProfileProvider = Arc::new(move |command| {
        let runtime = remote_profile_runtime.clone();
        let future: RemoteProfileProviderFuture = Box::pin(async move {
            runtime.profile_command(command).await.map_err(|error| {
                SnapshotProviderError::new(error.code, error.reason, error.retryable)
            })
        });
        future
    });
    let secret_command_provider: SecretCommandProvider = Arc::new(move |command| {
        let runtime = secret_runtime.clone();
        let future: SecretCommandProviderFuture = Box::pin(async move {
            runtime.secret_command(command).await.map_err(|error| {
                SnapshotProviderError::new(error.code, error.reason, error.retryable)
            })
        });
        future
    });
    let remote_session_provider: RemoteSessionProvider = Arc::new(move |command| {
        let runtime = remote_session_runtime.clone();
        let future: RemoteSessionProviderFuture = Box::pin(async move {
            runtime.session_command(command).await.map_err(|error| {
                SnapshotProviderError::new(error.code, error.reason, error.retryable)
            })
        });
        future
    });
    let terminal_provider: TerminalProvider = Arc::new(move |command| {
        let runtime = terminal_runtime.clone();
        let future: TerminalProviderFuture = Box::pin(async move {
            runtime.terminal_command(command).await.map_err(|error| {
                SnapshotProviderError::new(error.code, error.reason, error.retryable)
            })
        });
        future
    });
    let transfer_provider: TransferProvider = Arc::new(move |command| {
        let runtime = transfer_runtime.clone();
        let future: TransferProviderFuture = Box::pin(async move {
            runtime.transfer_command(command).await.map_err(|error| {
                SnapshotProviderError::new(error.code, error.reason, error.retryable)
            })
        });
        future
    });
    let transfer_local_handle_provider: TransferLocalHandleProvider = Arc::new(move |bind| {
        let runtime = transfer_local_handle_runtime.clone();
        let future: TransferLocalHandleProviderFuture = Box::pin(async move {
            runtime
                .bind_transfer_local_handle(bind)
                .await
                .map_err(|error| {
                    SnapshotProviderError::new(error.code, error.reason, error.retryable)
                })
        });
        future
    });
    let notes_provider: NotesProvider = Arc::new(move |command| {
        let handle = notes_handle.clone();
        let future: NotesProviderFuture = Box::pin(async move {
            handle.execute(command).await.map_err(|error| {
                SnapshotProviderError::new(error.code, error.reason, error.retryable)
            })
        });
        future
    });

    let speedtest_basic_handle = speedtest_handle.clone();
    let speedtest_provider: SpeedTestProvider = Arc::new(move |stages| {
        let handle = speedtest_basic_handle.clone();
        let future: SpeedTestProviderFuture = Box::pin(async move {
            handle
                .start_basic(stages)
                .map_err(|error| SnapshotProviderError::new(error.code, error.reason, error.retryable))
        });
        future
    });
    let speedtest_cancel_handle = speedtest_handle.clone();
    let speedtest_cancel_provider: SpeedTestCancelProvider = Arc::new(move || {
        let handle = speedtest_cancel_handle.clone();
        let future: SpeedTestCancelProviderFuture = Box::pin(async move {
            Ok(handle.cancel())
        });
        future
    });
    let speedtest_deep_handle = speedtest_handle.clone();
    let speedtest_deep_provider: SpeedTestDeepProvider = Arc::new(move |command| {
        let handle = speedtest_deep_handle.clone();
        let future: SpeedTestDeepProviderFuture = Box::pin(async move {
            handle.deep_command(command).await.map_err(|error| {
                SnapshotProviderError::new(error.code, error.reason, error.retryable)
            })
        });
        future
    });

    ServerConfig::new(DAEMON_VERSION, capability_provider)
        .with_snapshot_provider(snapshot_provider)
        .with_network_snapshot_provider(network_snapshot_provider)
        .with_usage_summary_provider(usage_summary_provider)
        .with_remote_capabilities_provider(remote_capabilities_provider)
        .with_system_info_provider(system_info_provider)
        .with_remote_profile_provider(remote_profile_provider)
        .with_secret_command_provider(secret_command_provider)
        .with_remote_session_provider(remote_session_provider)
        .with_terminal_provider(terminal_provider)
        .with_transfer_provider(transfer_provider)
        .with_transfer_local_handle_provider(transfer_local_handle_provider)
        .with_notes_provider(notes_provider)
        .with_speedtest_provider(speedtest_provider)
        .with_speedtest_cancel_provider(speedtest_cancel_provider)
        .with_speedtest_deep_provider(speedtest_deep_provider)
}

pub async fn serve_appd(
    listener: UnixListener,
    telemetry_handle: TelemetryManagerHandle,
    network_handle: NetworkHandle,
    usage_handle: UsageHandle,
    notes_handle: NotesHandle,
    remote_runtime: RemoteRuntime,
    speedtest_handle: SpeedTestHandle,
    shutdown: watch::Receiver<bool>,
) -> Result<(), ServerError> {
    serve(
        listener,
        server_config(
            telemetry_handle,
            network_handle,
            usage_handle,
            notes_handle,
            remote_runtime,
            speedtest_handle,
        ),
        shutdown,
    )
    .await
}

pub fn capability_state(
    snapshot: Result<TelemetrySnapshot, localdesk_telemetry::StoreError>,
) -> CapabilityRuntimeState {
    let Ok(snapshot) = snapshot else {
        return CapabilityRuntimeState::unreachable("telemetry_store_unavailable");
    };

    match (snapshot.status, snapshot.freshness) {
        (TelemetryStatus::Complete, TelemetryFreshness::Fresh) => {
            CapabilityRuntimeState::healthy("telemetry_healthy")
        }
        (TelemetryStatus::Unavailable, TelemetryFreshness::Unknown) => {
            CapabilityRuntimeState::unreachable("telemetry_unavailable")
        }
        (_, TelemetryFreshness::WarmingUp) => {
            CapabilityRuntimeState::degraded("telemetry_warming_up")
        }
        (_, TelemetryFreshness::Stale) => CapabilityRuntimeState::degraded("telemetry_stale"),
        (_, TelemetryFreshness::Unknown) => CapabilityRuntimeState::degraded("telemetry_unknown"),
        (_, TelemetryFreshness::Fresh) => CapabilityRuntimeState::degraded("telemetry_partial"),
    }
}

fn store_error_reason(error: &localdesk_telemetry::StoreError) -> &'static str {
    match error {
        localdesk_telemetry::StoreError::Poisoned => "telemetry_store_poisoned",
        localdesk_telemetry::StoreError::ShuttingDown => "telemetry_shutting_down",
        localdesk_telemetry::StoreError::GenerationExhausted => "telemetry_generation_exhausted",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use localdesk_domain::CapabilityAvailability;

    #[test]
    fn capability_mapping_is_dynamic_and_does_not_use_static_available() {
        let mut snapshot = TelemetrySnapshot::unavailable("helper_missing");
        assert_eq!(
            capability_state(Ok(snapshot.clone())).status,
            CapabilityAvailability::Unreachable
        );

        snapshot.status = TelemetryStatus::Partial;
        snapshot.freshness = TelemetryFreshness::WarmingUp;
        assert_eq!(
            capability_state(Ok(snapshot.clone())).status,
            CapabilityAvailability::Degraded
        );

        snapshot.status = TelemetryStatus::Complete;
        snapshot.freshness = TelemetryFreshness::Fresh;
        assert_eq!(
            capability_state(Ok(snapshot)).status,
            CapabilityAvailability::Healthy
        );

        let mut stale = TelemetrySnapshot::unavailable("stale");
        stale.status = TelemetryStatus::Partial;
        stale.freshness = TelemetryFreshness::Stale;
        assert_eq!(
            capability_state(Ok(stale)).status,
            CapabilityAvailability::Degraded
        );
    }
}
