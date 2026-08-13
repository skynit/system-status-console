use localdesk_domain::{MetricState, TelemetryFreshness, TelemetryStatus};
use localdesk_telemetry::{
    MAX_STALE_MS, PublishResult, STALE_AFTER_MS, TelemetryManager, TelemetryStoreConfig,
};
use localdesk_telemetry_helper_protocol::{
    HelperError, HelperErrorCode, PrivateApplicationResourceRecord, PrivateCgroupRecord,
    PrivateGroupingResolution, PrivateMetric, PrivateMetricState, PrivateProcessIdentity,
    PrivateProcessRecord, PrivateSnapshot, PrivateSystemFdSnapshot,
};
use std::time::{Duration, Instant};

fn snapshot(total_cpu_jiffies: u64, issue: bool, unknown_rss: bool) -> PrivateSnapshot {
    PrivateSnapshot {
        boot_id: "boot".to_owned(),
        euid: 1000,
        captured_at_unix_ms: total_cpu_jiffies as i64,
        captured_at_monotonic_ns: PrivateMetric::known(total_cpu_jiffies * 100_000_000),
        total_cpu_jiffies,
        logical_cpu_count: 1,
        processes: vec![PrivateProcessRecord {
            identity: PrivateProcessIdentity {
                boot_id: "boot".to_owned(),
                pid: 10,
                start_time_ticks: 1,
                euid: 1000,
            },
            ppid: 1,
            comm: "fixture".to_owned(),
            exe_basename: Some("fixture".to_owned()),
            cgroup_content: "0::/user.slice".to_owned(),
            application_key: "unknown:opaque-fixture".to_owned(),
            desktop_entry_id: None,
            grouping_resolution: PrivateGroupingResolution::Unknown,
            cpu_jiffies: total_cpu_jiffies,
            rss_bytes: if unknown_rss {
                PrivateMetric::unavailable(PrivateMetricState::Unknown, "rss_unknown")
            } else {
                PrivateMetric::known(4096)
            },
            pss_bytes: PrivateMetric::known(3072),
            fd_used: PrivateMetric::known(2),
            fd_soft_limit: PrivateMetric::known(100),
            fd_percent_of_soft_limit: PrivateMetric::known(2.0),
        }],
        cgroups: vec![PrivateCgroupRecord {
            cgroup_path: "/user.slice".to_owned(),
            application_key: "unknown:opaque-fixture".to_owned(),
            cpu_usage_usec: PrivateMetric::known(total_cpu_jiffies * 10_000),
            memory_current_bytes: PrivateMetric::known(8192),
            process_count: PrivateMetric::known(1),
        }],
        applications: vec![PrivateApplicationResourceRecord {
            application_key: "unknown:opaque-fixture".to_owned(),
            process_count: 1,
            proc_cpu_jiffies_sum: PrivateMetric::known(total_cpu_jiffies),
            rss_sum_bytes: if unknown_rss {
                PrivateMetric::unavailable(PrivateMetricState::Unknown, "rss_unknown")
            } else {
                PrivateMetric::known(4096)
            },
            pss_sum_bytes: PrivateMetric::known(3072),
            fd_used_sum: PrivateMetric::known(2),
            fd_soft_limit_sum: PrivateMetric::known(100),
            fd_percent_of_attributed_sum: PrivateMetric::known(100.0),
            fd_percent_of_soft_limit_sum: PrivateMetric::known(2.0),
            cgroup_cpu_usage_usec: PrivateMetric::known(total_cpu_jiffies * 10_000),
            memory_current_bytes: PrivateMetric::known(8192),
            cgroup_process_count: PrivateMetric::known(1),
        }],
        system_fd: PrivateSystemFdSnapshot {
            file_nr_allocated: PrivateMetric::known(10),
            file_nr_max: PrivateMetric::known(0),
            file_max: PrivateMetric::known(100),
            pressure_percent: PrivateMetric::known(10.0),
        },
        excluded_other_uid: 0,
        skipped_race: 0,
        permission_denied_counts: Vec::new(),
        issues: issue
            .then(|| {
                localdesk_telemetry_helper_protocol::PrivateIssueCount::new("fixture_issue", 1)
            })
            .into_iter()
            .collect(),
    }
}

#[test]
fn manager_tracks_warming_fresh_partial_and_drops_late_generation() {
    let mut manager = TelemetryManager::with_defaults();
    let initial = manager.handle().snapshot().expect("initial snapshot");
    assert_eq!(initial.status, TelemetryStatus::Unavailable);
    assert_eq!(initial.freshness, TelemetryFreshness::Unknown);
    assert!(initial.applications.is_empty());

    let first_generation = manager.begin_sample().expect("first generation");
    assert_eq!(
        manager
            .accept_snapshot(first_generation, &snapshot(10, false, false))
            .expect("warming publish"),
        PublishResult::Published
    );
    let warming = manager.handle().snapshot().expect("warming snapshot");
    assert_eq!(warming.freshness, TelemetryFreshness::WarmingUp);
    assert_eq!(warming.status, TelemetryStatus::Partial);

    let second_generation = manager.begin_sample().expect("second generation");
    let measured = manager
        .accept_snapshot(second_generation, &snapshot(20, false, false))
        .expect("fresh publish");
    assert_eq!(measured, PublishResult::Published);
    let fresh = manager.handle().snapshot().expect("fresh snapshot");
    assert_eq!(fresh.freshness, TelemetryFreshness::Fresh);
    assert_eq!(fresh.status, TelemetryStatus::Complete);
    assert_eq!(fresh.applications[0].rss_sum_bytes.value, Some(4096));
    assert_eq!(fresh.applications[0].pss_sum_bytes.value, Some(3072));
    assert_eq!(
        fresh.applications[0]
            .cgroup_cpu_percent_total_capacity
            .value,
        Some(10.0)
    );
    assert_eq!(fresh.system_fd.pressure_percent.value, Some(10.0));
    assert!(fresh.last_success_at_unix_ms.is_some());
    let bindings = manager.handle().cgroup_bindings().expect("cgroup bindings");
    assert!(bindings.available);
    assert_eq!(bindings.bindings.len(), 1);
    assert_eq!(bindings.bindings[0].cgroup_path, "/user.slice");
    assert_eq!(
        bindings.bindings[0].application_key,
        "unknown:opaque-fixture"
    );

    let third_generation = manager.begin_sample().expect("third generation");
    let fourth_generation = manager.begin_sample().expect("fourth generation");
    assert_eq!(
        manager
            .accept_snapshot(third_generation, &snapshot(30, false, false))
            .expect("late reply"),
        PublishResult::DroppedLateGeneration
    );
    assert_eq!(
        manager.handle().current_generation().expect("generation"),
        fourth_generation
    );
    manager
        .accept_snapshot(fourth_generation, &snapshot(30, true, true))
        .expect("partial publish");
    let partial = manager.handle().snapshot().expect("partial snapshot");
    assert_eq!(partial.status, TelemetryStatus::Partial);
    assert_eq!(partial.freshness, TelemetryFreshness::Fresh);
    assert_eq!(partial.applications[0].rss_sum_bytes.value, None);
    assert_eq!(
        partial.applications[0].rss_sum_bytes.state,
        MetricState::Unknown
    );
}

#[test]
fn store_retains_stale_data_then_expires_to_unavailable_without_zero_sentinels() {
    let config = TelemetryStoreConfig {
        stale_after: Duration::from_millis(STALE_AFTER_MS),
        max_stale: Duration::from_millis(MAX_STALE_MS),
    };
    let mut manager = TelemetryManager::new(config);
    let generation = manager.begin_sample().expect("generation");
    manager
        .accept_snapshot(generation, &snapshot(10, false, false))
        .expect("warming");
    let generation = manager.begin_sample().expect("generation");
    manager
        .accept_snapshot(generation, &snapshot(20, false, false))
        .expect("fresh");
    let error = HelperError::new(HelperErrorCode::ProcUnavailable, true, "proc_unavailable");
    let generation = manager.begin_sample().expect("error generation");
    manager
        .accept_collection_error(generation, error)
        .expect("error publish");
    let retained = manager.handle().snapshot().expect("retained snapshot");
    assert_eq!(retained.status, TelemetryStatus::Partial);
    assert_eq!(retained.freshness, TelemetryFreshness::Fresh);
    assert!(retained.retryable);
    assert_eq!(retained.applications.len(), 1);

    let stale_at = Instant::now() + Duration::from_millis(STALE_AFTER_MS + 1);
    let stale = manager
        .handle()
        .snapshot_at(stale_at)
        .expect("stale snapshot");
    assert_eq!(stale.status, TelemetryStatus::Partial);
    assert_eq!(stale.freshness, TelemetryFreshness::Stale);
    assert_eq!(stale.applications.len(), 1);
    assert!(stale.last_success_at_unix_ms.is_some());

    let expired_at = Instant::now() + Duration::from_millis(MAX_STALE_MS + 1);
    let expired = manager
        .handle()
        .snapshot_at(expired_at)
        .expect("expired snapshot");
    assert_eq!(expired.status, TelemetryStatus::Unavailable);
    assert_eq!(expired.freshness, TelemetryFreshness::Unknown);
    assert!(expired.applications.is_empty());
    assert!(expired.last_success_at_unix_ms.is_some());
    let expired_bindings = manager
        .handle()
        .cgroup_bindings_at(expired_at)
        .expect("expired bindings");
    assert!(!expired_bindings.available);
    assert_eq!(expired_bindings.reason, "telemetry_cgroup_bindings_stale");
    assert!(expired_bindings.bindings.is_empty());
    assert!(
        expired
            .applications
            .iter()
            .all(|application| { application.rss_sum_bytes.value.is_none() })
    );
}
