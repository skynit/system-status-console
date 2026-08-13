#[path = "../src/telemetry.rs"]
mod telemetry;

use localdesk_telemetry::{PublishResult, TelemetryManager};
use localdesk_telemetry_helper_protocol::{CollectionReply, HelperError, HelperErrorCode};
use std::{path::PathBuf, time::Duration};
use telemetry::{TelemetrySupervisor, store_config};
use tokio::{
    sync::{oneshot, watch},
    time::{sleep, timeout},
};

#[tokio::test]
async fn missing_helper_is_published_without_blocking_manager_startup() {
    let supervisor = TelemetrySupervisor::with_helper_path(
        TelemetryManager::new(store_config()),
        PathBuf::from("/definitely/missing/localdesk-telemetry-helper"),
    );
    let handle = supervisor.handle();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (ack_tx, ack_rx) = oneshot::channel();
    let task = tokio::spawn(supervisor.run(shutdown_rx, ack_tx));

    sleep(Duration::from_millis(50)).await;
    let snapshot = handle.snapshot().expect("snapshot");
    assert_eq!(snapshot.reason, "helper_missing");
    assert!(!snapshot.retryable);

    shutdown_tx.send(true).expect("shutdown");
    timeout(Duration::from_secs(2), ack_rx)
        .await
        .expect("kill acknowledgement")
        .expect("ack sender");
    timeout(Duration::from_secs(2), task)
        .await
        .expect("supervisor shutdown")
        .expect("supervisor join");
}

#[tokio::test]
async fn protocol_failure_kills_and_reaps_before_the_next_generation() {
    let supervisor = TelemetrySupervisor::with_helper_path(
        TelemetryManager::new(store_config()),
        PathBuf::from("/bin/cat"),
    );
    let handle = supervisor.handle();
    let reaper = supervisor.cleanup_handle();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (ack_tx, ack_rx) = oneshot::channel();
    let task = tokio::spawn(supervisor.run(shutdown_rx, ack_tx));

    sleep(Duration::from_millis(100)).await;
    let snapshot = handle.snapshot().expect("snapshot");
    assert_eq!(snapshot.reason, "helper_protocol_error");

    shutdown_tx.send(true).expect("shutdown");
    timeout(Duration::from_secs(2), ack_rx)
        .await
        .expect("kill acknowledgement")
        .expect("ack sender");
    timeout(Duration::from_secs(2), task)
        .await
        .expect("supervisor shutdown")
        .expect("supervisor join");
    assert!(!reaper.wait().await.expect("reaper state"));
}

#[test]
fn late_generation_is_dropped_without_changing_last_success() {
    let mut manager = TelemetryManager::new(store_config());
    let first = manager.begin_sample().expect("first generation");
    let _current = manager.begin_sample().expect("second generation");
    let reply = CollectionReply::error(
        first,
        HelperError::new(HelperErrorCode::Internal, true, "late_reply"),
    );

    assert_eq!(
        manager.accept_reply(reply).expect("accept reply"),
        PublishResult::DroppedLateGeneration
    );
    let snapshot = manager.handle().snapshot().expect("snapshot");
    assert_eq!(snapshot.reason, "collector_unavailable");
    assert_eq!(snapshot.last_success_at_unix_ms, None);
}

#[test]
fn sample_deadline_and_interval_are_bounded() {
    assert_eq!(telemetry::SAMPLE_INTERVAL, Duration::from_secs(1));
    assert_eq!(telemetry::SAMPLE_DEADLINE, Duration::from_secs(2));
}
