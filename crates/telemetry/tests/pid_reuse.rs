use localdesk_domain::MetricState;
use localdesk_telemetry::Sampler;
use localdesk_telemetry_helper_protocol::{
    PrivateGroupingResolution, PrivateMetric, PrivateProcessIdentity, PrivateProcessRecord,
    PrivateSnapshot, PrivateSystemFdSnapshot,
};

fn raw(start_time_ticks: u64, cpu_jiffies: u64, include_process: bool) -> PrivateSnapshot {
    let processes = include_process
        .then(|| PrivateProcessRecord {
            identity: PrivateProcessIdentity {
                boot_id: "boot".to_owned(),
                pid: 7,
                start_time_ticks,
                euid: 1000,
            },
            ppid: 1,
            comm: "fixture".to_owned(),
            exe_basename: Some("fixture".to_owned()),
            cgroup_content: "0::/user.slice".to_owned(),
            application_key: format!("unknown:7:{start_time_ticks}"),
            desktop_entry_id: None,
            grouping_resolution: PrivateGroupingResolution::Unknown,
            cpu_jiffies,
            rss_bytes: PrivateMetric::known(1),
            pss_bytes: PrivateMetric::known(1),
            fd_used: PrivateMetric::known(1),
            fd_soft_limit: PrivateMetric::known(10),
            fd_percent_of_soft_limit: PrivateMetric::known(10.0),
        })
        .into_iter()
        .collect();
    PrivateSnapshot {
        boot_id: "boot".to_owned(),
        euid: 1000,
        captured_at_unix_ms: 1,
        captured_at_monotonic_ns: PrivateMetric::known(cpu_jiffies * 100_000_000),
        total_cpu_jiffies: cpu_jiffies.saturating_mul(10),
        logical_cpu_count: 1,
        processes,
        cgroups: Vec::new(),
        applications: Vec::new(),
        system_fd: PrivateSystemFdSnapshot::unavailable(
            localdesk_telemetry_helper_protocol::PrivateMetricState::Unknown,
            "fixture_unknown",
        ),
        excluded_other_uid: 0,
        skipped_race: 0,
        permission_denied_counts: Vec::new(),
        issues: Vec::new(),
    }
}

#[test]
fn pid_reuse_does_not_reuse_old_cpu_delta() {
    let mut sampler = Sampler::new();
    let _ = sampler
        .reduce_snapshot(&raw(10, 10, true))
        .expect("warming");
    let reused = sampler
        .reduce_snapshot(&raw(11, 20, true))
        .expect("reused process");
    assert_eq!(reused.applications.len(), 1);
    assert_eq!(
        reused.applications[0].cpu_percent_total_capacity_sum.state,
        MetricState::WarmingUp
    );
    assert!(
        reused.applications[0]
            .cpu_percent_total_capacity_sum
            .value
            .is_none()
    );
}

#[test]
fn exited_process_has_no_stale_sample() {
    let mut sampler = Sampler::new();
    let _ = sampler
        .reduce_snapshot(&raw(10, 10, true))
        .expect("warming");
    let exited = sampler
        .reduce_snapshot(&raw(10, 20, false))
        .expect("exited process");
    assert!(exited.applications.is_empty());
}
