use crate::procfs::{
    ProcessIdentity, RawApplicationResourceRecord, RawProcess, RawSnapshot, desktop_display_name,
    desktop_roots_from_env, raw_from_protocol,
};
use localdesk_domain::{
    ApplicationSample, GroupingResolution, IssueCount, MetricState, MetricValue, SystemFdSample,
    TELEMETRY_SCHEMA_VERSION, TELEMETRY_SCOPE_FULL_CGROUP, TELEMETRY_SCOPE_SAME_EUID,
    TELEMETRY_SCOPE_SYSTEM, TelemetryFreshness, TelemetrySnapshot, TelemetryStatus,
};
use localdesk_telemetry_helper_protocol::{
    MAX_APPLICATION_RECORDS, MAX_PROCESS_RECORDS, PrivateSnapshot,
};
use std::{collections::HashMap, path::PathBuf};
use thiserror::Error;
use uuid::Uuid;

pub const SAMPLE_INTERVAL_MS: u64 = 1_000;
pub const MIN_SAMPLE_INTERVAL_MS: u64 = 750;
pub const MAX_SAMPLE_INTERVAL_MS: u64 = 2_500;

#[derive(Debug, Error)]
pub enum SamplerError {
    #[error("private telemetry snapshot is invalid: {0}")]
    InvalidSnapshot(String),
    #[error("process record count exceeds {MAX_PROCESS_RECORDS}")]
    ProcessLimitExceeded,
    #[error("application record count exceeds {MAX_APPLICATION_RECORDS}")]
    ApplicationLimitExceeded,
}

#[derive(Debug, Clone)]
struct Baseline {
    boot_id: String,
    captured_at_monotonic_ns: MetricValue<u64>,
    total_cpu_jiffies: u64,
    process_cpu: HashMap<ProcessIdentity, u64>,
    application_cgroup_cpu_usec: HashMap<String, u64>,
}

pub struct Sampler {
    baseline: Option<Baseline>,
    desktop_roots: Option<Vec<PathBuf>>,
}

pub type TelemetryReducer = Sampler;

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Sampler {
    pub fn new() -> Self {
        Self {
            baseline: None,
            desktop_roots: None,
        }
    }

    /// Restrict desktop-entry display-name lookups to the given roots instead
    /// of the process environment (used by tests with fixture desktop files).
    pub fn with_desktop_roots(roots: Vec<PathBuf>) -> Self {
        Self {
            baseline: None,
            desktop_roots: Some(roots),
        }
    }

    pub fn reset(&mut self) {
        self.baseline = None;
    }

    pub fn reduce_snapshot(
        &mut self,
        snapshot: &PrivateSnapshot,
    ) -> Result<TelemetrySnapshot, SamplerError> {
        let raw = raw_from_protocol(snapshot)
            .map_err(|error| SamplerError::InvalidSnapshot(error.to_string()))?;
        self.reduce_raw(raw)
    }

    pub(crate) fn reduce_raw(
        &mut self,
        raw: RawSnapshot,
    ) -> Result<TelemetrySnapshot, SamplerError> {
        if raw.processes.len() > MAX_PROCESS_RECORDS {
            return Err(SamplerError::ProcessLimitExceeded);
        }
        self.observe(raw)
    }

    fn observe(&mut self, raw: RawSnapshot) -> Result<TelemetrySnapshot, SamplerError> {
        let desktop_roots = self
            .desktop_roots
            .clone()
            .unwrap_or_else(desktop_roots_from_env);
        let build = |raw: &RawSnapshot,
                     cpu: Option<(&Baseline, Option<u64>)>,
                     sample_interval_ms: Option<u64>,
                     capture_interval_ns: Option<u64>,
                     status: TelemetryStatus,
                     reason: &str| {
            build_snapshot(
                raw,
                cpu,
                sample_interval_ms,
                capture_interval_ns,
                status,
                reason,
                &desktop_roots,
            )
        };
        let process_cpu = raw
            .processes
            .iter()
            .map(|process| (process.identity.clone(), process.cpu_jiffies))
            .collect::<HashMap<_, _>>();
        let application_cgroup_cpu_usec = raw
            .applications
            .iter()
            .filter_map(|application| {
                application
                    .cgroup_cpu_usage_usec
                    .value
                    .map(|value| (application.application_key.clone(), value))
            })
            .collect::<HashMap<_, _>>();
        let Some(previous) = self.baseline.take() else {
            self.baseline = Some(Baseline {
                boot_id: raw.boot_id.clone(),
                captured_at_monotonic_ns: raw.captured_at_monotonic_ns.clone(),
                total_cpu_jiffies: raw.total_cpu_jiffies,
                process_cpu,
                application_cgroup_cpu_usec,
            });
            return build(
                &raw,
                None,
                None,
                None,
                TelemetryStatus::Partial,
                "warming_up",
            );
        };

        if previous.boot_id != raw.boot_id {
            self.baseline = Some(Baseline {
                boot_id: raw.boot_id.clone(),
                captured_at_monotonic_ns: raw.captured_at_monotonic_ns.clone(),
                total_cpu_jiffies: raw.total_cpu_jiffies,
                process_cpu,
                application_cgroup_cpu_usec,
            });
            return build(
                &raw,
                None,
                None,
                None,
                TelemetryStatus::Partial,
                "boot_changed",
            );
        }

        self.baseline = Some(Baseline {
            boot_id: raw.boot_id.clone(),
            captured_at_monotonic_ns: raw.captured_at_monotonic_ns.clone(),
            total_cpu_jiffies: raw.total_cpu_jiffies,
            process_cpu,
            application_cgroup_cpu_usec,
        });

        let (capture_interval_ns, interval) = match capture_interval(
            &previous.captured_at_monotonic_ns,
            &raw.captured_at_monotonic_ns,
        ) {
            Ok(interval) => interval,
            Err((state, reason)) => {
                return build(
                    &raw,
                    None,
                    None,
                    None,
                    TelemetryStatus::Partial,
                    reason_for_interval_state(state, reason),
                );
            }
        };
        let minimum_ns = MIN_SAMPLE_INTERVAL_MS.saturating_mul(1_000_000);
        let maximum_ns = MAX_SAMPLE_INTERVAL_MS.saturating_mul(1_000_000);
        if !(minimum_ns..=maximum_ns).contains(&capture_interval_ns) {
            return build(
                &raw,
                None,
                Some(interval),
                Some(capture_interval_ns),
                TelemetryStatus::Partial,
                "sampling_gap",
            );
        }

        let total_delta = raw
            .total_cpu_jiffies
            .checked_sub(previous.total_cpu_jiffies);
        let Some(total_delta) = total_delta.filter(|delta| *delta > 0) else {
            return build(
                &raw,
                Some((&previous, None)),
                Some(interval),
                Some(capture_interval_ns),
                TelemetryStatus::Partial,
                "cpu_denominator_zero",
            );
        };
        build(
            &raw,
            Some((&previous, Some(total_delta))),
            Some(interval),
            Some(capture_interval_ns),
            if raw.issues.is_empty() && raw.skipped_race == 0 {
                TelemetryStatus::Complete
            } else {
                TelemetryStatus::Partial
            },
            if raw.issues.is_empty() && raw.skipped_race == 0 {
                "complete"
            } else {
                "partial_metrics"
            },
        )
    }
}

fn capture_interval(
    previous: &MetricValue<u64>,
    current: &MetricValue<u64>,
) -> Result<(u64, u64), (MetricState, &'static str)> {
    let unavailable = dominant_unavailable_state([previous, current].into_iter());
    if unavailable != MetricState::Known {
        return Err((unavailable, "capture_interval_unavailable"));
    }
    let Some(previous) = previous.value else {
        return Err((MetricState::Unknown, "capture_interval_unavailable"));
    };
    let Some(current) = current.value else {
        return Err((MetricState::Unknown, "capture_interval_unavailable"));
    };
    let Some(interval_ns) = current
        .checked_sub(previous)
        .filter(|interval| *interval > 0)
    else {
        return Err((MetricState::SamplingGap, "capture_interval_non_monotonic"));
    };
    let interval_ms = interval_ns.saturating_add(500_000) / 1_000_000;
    Ok((interval_ns, interval_ms))
}

fn reason_for_interval_state(state: MetricState, fallback: &'static str) -> &'static str {
    match state {
        MetricState::PermissionDenied => "capture_interval_permission_denied",
        MetricState::Raced => "capture_interval_raced",
        MetricState::Unbounded => "capture_interval_unbounded",
        MetricState::SamplingGap => "capture_interval_non_monotonic",
        MetricState::WarmingUp => "capture_interval_warming_up",
        MetricState::Unknown | MetricState::Known => fallback,
    }
}

fn cpu_unavailable_state(reason: &str) -> MetricState {
    match reason {
        "warming_up" | "boot_changed" | "capture_interval_warming_up" => MetricState::WarmingUp,
        "sampling_gap" | "cpu_denominator_zero" | "capture_interval_non_monotonic" => {
            MetricState::SamplingGap
        }
        "capture_interval_permission_denied" => MetricState::PermissionDenied,
        "capture_interval_raced" => MetricState::Raced,
        "capture_interval_unbounded" => MetricState::Unbounded,
        _ => MetricState::Unknown,
    }
}

#[derive(Debug, Clone)]
struct PrivateProcessSample {
    exe_basename: Option<String>,
    application_key: String,
    grouping_resolution: GroupingResolution,
    cpu_percent_total_capacity: MetricValue<f64>,
    rss_bytes: MetricValue<u64>,
    pss_bytes: MetricValue<u64>,
    fd_used: MetricValue<u64>,
    fd_soft_limit: MetricValue<u64>,
    fd_percent_of_soft_limit: MetricValue<f64>,
}

fn build_snapshot(
    raw: &RawSnapshot,
    cpu: Option<(&Baseline, Option<u64>)>,
    sample_interval_ms: Option<u64>,
    capture_interval_ns: Option<u64>,
    mut status: TelemetryStatus,
    reason: &str,
    desktop_roots: &[PathBuf],
) -> Result<TelemetrySnapshot, SamplerError> {
    let processes = raw
        .processes
        .iter()
        .map(|process| process_sample(process, cpu, reason))
        .collect::<Vec<_>>();
    let applications = aggregate_applications(
        &raw.processes,
        &processes,
        &raw.applications,
        SamplingContext {
            baseline: cpu.map(|(baseline, _)| baseline),
            capture_interval_ns,
            logical_cpu_count: raw.logical_cpu_count,
            reason,
            desktop_roots,
        },
    )?;
    let mut issues = raw.issues.clone();
    let mut permission_denied_counts = raw.permission_denied_counts.clone();
    if reason.starts_with("capture_interval_") {
        ensure_issue(&mut issues, reason);
        if cpu_unavailable_state(reason) == MetricState::PermissionDenied {
            ensure_issue(&mut permission_denied_counts, reason);
        }
    }
    for process in &processes {
        for metric in [
            &process.rss_bytes,
            &process.pss_bytes,
            &process.fd_used,
            &process.fd_soft_limit,
        ] {
            if metric.state != MetricState::Known {
                add_issue(
                    &mut issues,
                    metric.reason.as_deref().unwrap_or("metric_unknown"),
                );
                if metric.state == MetricState::PermissionDenied {
                    add_issue(
                        &mut permission_denied_counts,
                        metric
                            .reason
                            .as_deref()
                            .unwrap_or("metric_permission_denied"),
                    );
                }
                status = TelemetryStatus::Partial;
            }
        }
        if process.fd_percent_of_soft_limit.state != MetricState::Known {
            add_issue(
                &mut issues,
                process
                    .fd_percent_of_soft_limit
                    .reason
                    .as_deref()
                    .unwrap_or("metric_unknown"),
            );
            if process.fd_percent_of_soft_limit.state == MetricState::PermissionDenied {
                add_issue(
                    &mut permission_denied_counts,
                    process
                        .fd_percent_of_soft_limit
                        .reason
                        .as_deref()
                        .unwrap_or("fd_percent_permission_denied"),
                );
            }
            status = TelemetryStatus::Partial;
        }
        if process.cpu_percent_total_capacity.state != MetricState::Known {
            status = TelemetryStatus::Partial;
        }
    }
    issues.sort_by(|left, right| left.code.cmp(&right.code));
    permission_denied_counts.sort_by(|left, right| left.code.cmp(&right.code));
    for application in &applications {
        for state in [
            application.pss_sum_bytes.state,
            application.fd_percent_of_attributed_sum.state,
            application.cgroup_cpu_percent_total_capacity.state,
            application.memory_current_bytes.state,
            application.cgroup_process_count.state,
        ] {
            if state != MetricState::Known {
                status = TelemetryStatus::Partial;
            }
        }
    }
    for state in [
        raw.system_fd.file_nr_allocated.state,
        raw.system_fd.file_nr_max.state,
        raw.system_fd.file_max.state,
        raw.system_fd.pressure_percent.state,
    ] {
        if state != MetricState::Known {
            status = TelemetryStatus::Partial;
        }
    }
    let final_reason = if status == TelemetryStatus::Complete {
        "complete"
    } else {
        reason
    };
    let freshness = match reason {
        "warming_up" | "boot_changed" => TelemetryFreshness::WarmingUp,
        _ => TelemetryFreshness::Fresh,
    };
    Ok(TelemetrySnapshot {
        schema_version: TELEMETRY_SCHEMA_VERSION,
        snapshot_id: Uuid::new_v4(),
        captured_at_unix_ms: Some(raw.captured_at_unix_ms),
        sample_interval_ms,
        logical_cpu_count: (raw.logical_cpu_count > 0).then_some(raw.logical_cpu_count),
        freshness,
        status,
        reason: final_reason.to_owned(),
        retryable: false,
        scope: TELEMETRY_SCOPE_SAME_EUID.to_owned(),
        last_success_at_unix_ms: Some(raw.captured_at_unix_ms),
        permission_denied_counts,
        issues,
        system_fd: SystemFdSample {
            scope: TELEMETRY_SCOPE_SYSTEM.to_owned(),
            file_nr_allocated: raw.system_fd.file_nr_allocated.clone(),
            file_nr_max: raw.system_fd.file_nr_max.clone(),
            file_max: raw.system_fd.file_max.clone(),
            pressure_percent: raw.system_fd.pressure_percent.clone(),
        },
        applications,
    })
}

fn process_sample(
    process: &RawProcess,
    cpu: Option<(&Baseline, Option<u64>)>,
    reason: &str,
) -> PrivateProcessSample {
    let cpu_percent_total_capacity = match cpu {
        None => MetricValue::unavailable(cpu_unavailable_state(reason), reason),
        Some((baseline, Some(total_delta))) => match baseline.process_cpu.get(&process.identity) {
            Some(previous) => match process.cpu_jiffies.checked_sub(*previous) {
                Some(delta) => MetricValue::known((delta as f64) * 100.0 / (total_delta as f64)),
                None => MetricValue::unavailable(MetricState::SamplingGap, "cpu_counter_reset"),
            },
            None => MetricValue::unavailable(MetricState::WarmingUp, "process_baseline_missing"),
        },
        Some((_, None)) => {
            MetricValue::unavailable(MetricState::SamplingGap, "cpu_denominator_zero")
        }
    };
    PrivateProcessSample {
        exe_basename: process.exe_basename.clone(),
        application_key: process.application_key.clone(),
        grouping_resolution: process.grouping_resolution,
        cpu_percent_total_capacity,
        rss_bytes: process.rss_bytes.clone(),
        pss_bytes: process.pss_bytes.clone(),
        fd_used: process.fd_used.clone(),
        fd_soft_limit: process.fd_soft_limit.clone(),
        fd_percent_of_soft_limit: process.fd_percent_of_soft_limit.clone(),
    }
}

struct SamplingContext<'a> {
    baseline: Option<&'a Baseline>,
    capture_interval_ns: Option<u64>,
    logical_cpu_count: u32,
    reason: &'a str,
    desktop_roots: &'a [PathBuf],
}

fn aggregate_applications(
    raw: &[RawProcess],
    processes: &[PrivateProcessSample],
    resources: &[RawApplicationResourceRecord],
    context: SamplingContext<'_>,
) -> Result<Vec<ApplicationSample>, SamplerError> {
    let mut groups = HashMap::<String, Vec<(&RawProcess, &PrivateProcessSample)>>::new();
    for (raw, sample) in raw.iter().zip(processes) {
        groups
            .entry(sample.application_key.clone())
            .or_default()
            .push((raw, sample));
    }
    let SamplingContext {
        baseline,
        capture_interval_ns,
        logical_cpu_count,
        reason,
        desktop_roots,
    } = context;
    if groups.len() > MAX_APPLICATION_RECORDS {
        return Err(SamplerError::ApplicationLimitExceeded);
    }
    let resources = resources
        .iter()
        .map(|resource| (resource.application_key.as_str(), resource))
        .collect::<HashMap<_, _>>();
    let mut applications = groups
        .into_iter()
        .map(|(application_key, members)| {
            let resource = resources.get(application_key.as_str()).copied();
            ApplicationSample {
                grouping_resolution: members
                    .iter()
                    .map(|(_, sample)| sample.grouping_resolution)
                    .max_by_key(|resolution| grouping_priority(*resolution))
                    .unwrap_or(GroupingResolution::Unknown),
                desktop_entry_id: members
                    .iter()
                    .find_map(|(raw, _)| raw.desktop_entry_id.clone()),
                display_label: members
                    .iter()
                    .find_map(|(raw, _)| raw.desktop_entry_id.as_deref())
                    .and_then(|id| desktop_display_name(id, desktop_roots))
                    .or_else(|| {
                        members
                            .iter()
                            .find_map(|(raw, _)| raw.desktop_entry_id.clone())
                    })
                    .or_else(|| representative_executable_label(&members))
                    .unwrap_or_else(|| application_key.clone()),
                process_count: resource
                    .map(|resource| resource.process_count)
                    .unwrap_or(members.len() as u64),
                process_scope: TELEMETRY_SCOPE_SAME_EUID.to_owned(),
                cgroup_scope: TELEMETRY_SCOPE_FULL_CGROUP.to_owned(),
                cpu_percent_total_capacity_sum: sum_metric(
                    members
                        .iter()
                        .map(|(_, sample)| &sample.cpu_percent_total_capacity),
                    "application_cpu_unknown",
                ),
                rss_sum_bytes: sum_metric(
                    members.iter().map(|(_, sample)| &sample.rss_bytes),
                    "application_rss_unknown",
                ),
                pss_sum_bytes: resource
                    .map(|resource| resource.pss_sum_bytes.clone())
                    .unwrap_or_else(|| {
                        MetricValue::unavailable(MetricState::Unknown, "application_pss_missing")
                    }),
                fd_used_sum: sum_metric(
                    members.iter().map(|(_, sample)| &sample.fd_used),
                    "application_fd_used_unknown",
                ),
                fd_soft_limit_sum: sum_metric(
                    members.iter().map(|(_, sample)| &sample.fd_soft_limit),
                    "application_fd_limit_unknown",
                ),
                fd_percent_of_attributed_sum: resource
                    .map(|resource| resource.fd_percent_of_attributed_sum.clone())
                    .unwrap_or_else(|| {
                        MetricValue::unavailable(
                            MetricState::Unknown,
                            "application_fd_attributed_percent_missing",
                        )
                    }),
                fd_percent_of_soft_limit_sum: aggregate_fd_percent(&members),
                fd_max_process_percent_of_soft_limit: max_fd_percent(
                    members
                        .iter()
                        .map(|(_, sample)| &sample.fd_percent_of_soft_limit),
                ),
                cgroup_cpu_percent_total_capacity: cgroup_cpu_percent(
                    resource,
                    baseline,
                    capture_interval_ns,
                    logical_cpu_count,
                    reason,
                ),
                memory_current_bytes: resource
                    .map(|resource| resource.memory_current_bytes.clone())
                    .unwrap_or_else(|| {
                        MetricValue::unavailable(
                            MetricState::Unknown,
                            "application_memory_current_missing",
                        )
                    }),
                cgroup_process_count: resource
                    .map(|resource| resource.cgroup_process_count.clone())
                    .unwrap_or_else(|| {
                        MetricValue::unavailable(
                            MetricState::Unknown,
                            "application_cgroup_process_count_missing",
                        )
                    }),
                application_key,
            }
        })
        .collect::<Vec<_>>();
    applications.sort_by(|left, right| left.application_key.cmp(&right.application_key));
    Ok(applications)
}

fn representative_executable_label(
    members: &[(&RawProcess, &PrivateProcessSample)],
) -> Option<String> {
    let processes_by_pid = members
        .iter()
        .map(|(process, _)| (process.identity.pid, *process))
        .collect::<HashMap<_, _>>();

    // Prefer the shallowest workload process so a persistent shell launcher does not mask it.
    members
        .iter()
        .filter_map(|(process, sample)| {
            sample
                .exe_basename
                .as_ref()
                .filter(|name| !name.is_empty())
                .map(|name| (process, name))
        })
        .min_by_key(|(process, name)| {
            (
                is_shell_launcher(name),
                process_tree_depth(process, &processes_by_pid, members.len()),
                process.identity.start_time_ticks,
                process.identity.pid,
            )
        })
        .map(|(_, name)| name.clone())
}

fn process_tree_depth(
    process: &RawProcess,
    processes_by_pid: &HashMap<u32, &RawProcess>,
    member_count: usize,
) -> usize {
    let mut depth = 0;
    let mut parent_pid = process.ppid;
    for _ in 0..member_count {
        let Some(parent) = processes_by_pid.get(&parent_pid) else {
            break;
        };
        depth += 1;
        parent_pid = parent.ppid;
    }
    depth
}

fn is_shell_launcher(name: &str) -> bool {
    matches!(
        name,
        "sh" | "bash" | "dash" | "zsh" | "fish" | "ksh" | "mksh" | "csh" | "tcsh" | "nu"
    )
}

fn cgroup_cpu_percent(
    resource: Option<&RawApplicationResourceRecord>,
    baseline: Option<&Baseline>,
    capture_interval_ns: Option<u64>,
    logical_cpu_count: u32,
    reason: &str,
) -> MetricValue<f64> {
    let Some(resource) = resource else {
        return MetricValue::unavailable(MetricState::Unknown, "application_cgroup_cpu_missing");
    };
    let Some(current) = resource.cgroup_cpu_usage_usec.value else {
        let state = if resource.cgroup_cpu_usage_usec.state == MetricState::Known {
            MetricState::Unknown
        } else {
            resource.cgroup_cpu_usage_usec.state
        };
        return MetricValue::unavailable(
            state,
            resource
                .cgroup_cpu_usage_usec
                .reason
                .clone()
                .unwrap_or_else(|| "application_cgroup_cpu_unknown".to_owned()),
        );
    };
    if capture_interval_ns.is_none() {
        return MetricValue::unavailable(cpu_unavailable_state(reason), reason);
    }
    let Some(baseline) = baseline else {
        return MetricValue::unavailable(MetricState::WarmingUp, "cgroup_cpu_warming_up");
    };
    let Some(previous) = baseline
        .application_cgroup_cpu_usec
        .get(&resource.application_key)
    else {
        return MetricValue::unavailable(MetricState::WarmingUp, "cgroup_cpu_baseline_missing");
    };
    let Some(delta) = current.checked_sub(*previous) else {
        return MetricValue::unavailable(MetricState::SamplingGap, "cgroup_cpu_counter_reset");
    };
    let interval_ns = capture_interval_ns.expect("capture interval checked");
    if logical_cpu_count == 0 {
        return MetricValue::unavailable(MetricState::Unknown, "logical_cpu_count_unknown");
    }
    let denominator_usec = (interval_ns as f64) / 1_000.0 * (logical_cpu_count as f64);
    MetricValue::known((delta as f64) * 100.0 / denominator_usec)
}

fn grouping_priority(resolution: GroupingResolution) -> u8 {
    match resolution {
        GroupingResolution::DesktopEntryExact => 4,
        GroupingResolution::CgroupScope => 3,
        GroupingResolution::InheritedParent => 2,
        GroupingResolution::Unknown => 1,
    }
}

fn sum_metric<'a, T, I>(metrics: I, reason: &str) -> MetricValue<T>
where
    T: 'a + Copy + Default + std::ops::Add<Output = T>,
    I: Iterator<Item = &'a MetricValue<T>>,
{
    let mut total = T::default();
    let mut unavailable = MetricState::Known;
    for metric in metrics {
        if let Some(value) = metric.value.filter(|_| metric.state == MetricState::Known) {
            total = total + value;
        } else {
            unavailable = dominant_state(unavailable, normalized_unavailable_state(metric));
        }
    }
    if unavailable == MetricState::Known {
        MetricValue::known(total)
    } else {
        MetricValue::unavailable(unavailable, reason)
    }
}

fn aggregate_fd_percent(members: &[(&RawProcess, &PrivateProcessSample)]) -> MetricValue<f64> {
    let used = members.iter().map(|(_, sample)| &sample.fd_used);
    let limit = members.iter().map(|(_, sample)| &sample.fd_soft_limit);
    let used = sum_metric(used, "application_fd_used_unknown");
    let limit = sum_metric(limit, "application_fd_limit_unknown");
    match (used.value, limit.value) {
        (Some(used), Some(limit)) if limit > 0 => {
            MetricValue::known(used as f64 * 100.0 / limit as f64)
        }
        _ => {
            let state = dominant_unavailable_state(
                members
                    .iter()
                    .flat_map(|(_, sample)| [&sample.fd_used, &sample.fd_soft_limit]),
            );
            MetricValue::unavailable(state, "application_fd_percent_unknown")
        }
    }
}

fn max_fd_percent<'a, I>(metrics: I) -> MetricValue<f64>
where
    I: Iterator<Item = &'a MetricValue<f64>>,
{
    let mut maximum = None::<f64>;
    let mut unavailable = MetricState::Known;
    for metric in metrics {
        match metric.value.filter(|_| metric.state == MetricState::Known) {
            Some(value) => maximum = Some(maximum.map_or(value, |current| current.max(value))),
            None => {
                unavailable = dominant_state(unavailable, normalized_unavailable_state(metric));
            }
        }
    }
    if unavailable != MetricState::Known {
        MetricValue::unavailable(unavailable, "application_fd_max_process_percent_unknown")
    } else if let Some(maximum) = maximum {
        MetricValue::known(maximum)
    } else {
        MetricValue::unavailable(
            MetricState::Unknown,
            "application_fd_max_process_percent_unknown",
        )
    }
}

fn dominant_unavailable_state<'a, T, I>(metrics: I) -> MetricState
where
    T: 'a,
    I: Iterator<Item = &'a MetricValue<T>>,
{
    metrics.fold(MetricState::Known, |state, metric| {
        dominant_state(state, normalized_unavailable_state(metric))
    })
}

fn normalized_unavailable_state<T>(metric: &MetricValue<T>) -> MetricState {
    if metric.state == MetricState::Known && metric.value.is_some() {
        MetricState::Known
    } else if metric.state == MetricState::Known {
        MetricState::Unknown
    } else {
        metric.state
    }
}

fn dominant_state(left: MetricState, right: MetricState) -> MetricState {
    if unavailable_priority(left) >= unavailable_priority(right) {
        left
    } else {
        right
    }
}

fn unavailable_priority(state: MetricState) -> u8 {
    match state {
        MetricState::Known => 0,
        MetricState::Unknown => 1,
        MetricState::WarmingUp => 2,
        MetricState::SamplingGap => 3,
        MetricState::Unbounded => 4,
        MetricState::Raced => 5,
        MetricState::PermissionDenied => 6,
    }
}

fn add_issue(issues: &mut Vec<IssueCount>, code: &str) {
    if let Some(issue) = issues.iter_mut().find(|issue| issue.code == code) {
        issue.count = issue.count.saturating_add(1);
    } else {
        issues.push(IssueCount::new(code, 1));
    }
}

fn ensure_issue(issues: &mut Vec<IssueCount>, code: &str) {
    if !issues.iter().any(|issue| issue.code == code) {
        issues.push(IssueCount::new(code, 1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_reducer_is_order_independent_and_prefers_permission_denied() {
        let unknown = MetricValue::<u64>::unavailable(MetricState::Unknown, "unknown");
        let raced = MetricValue::<u64>::unavailable(MetricState::Raced, "raced");
        let denied =
            MetricValue::<u64>::unavailable(MetricState::PermissionDenied, "permission_denied");

        let forward = sum_metric([&unknown, &raced, &denied].into_iter(), "aggregate_unknown");
        let reverse = sum_metric([&denied, &raced, &unknown].into_iter(), "aggregate_unknown");

        assert_eq!(forward.state, MetricState::PermissionDenied);
        assert_eq!(reverse.state, MetricState::PermissionDenied);
    }

    #[test]
    fn max_process_fd_percent_exposes_risk_without_a_pid_and_requires_complete_inputs() {
        let low = MetricValue::known(0.0);
        let high = MetricValue::known(90.0);
        let maximum = max_fd_percent([&low, &high].into_iter());
        assert_eq!(maximum, MetricValue::known(90.0));

        let denied = MetricValue::unavailable(MetricState::PermissionDenied, "fd_denied");
        let incomplete = max_fd_percent([&low, &denied].into_iter());
        assert_eq!(incomplete.state, MetricState::PermissionDenied);
        assert_eq!(incomplete.value, None);
    }

    #[test]
    fn cgroup_cpu_uses_exact_monotonic_capture_interval() {
        let application_key = "app".to_owned();
        let baseline = Baseline {
            boot_id: "boot".to_owned(),
            captured_at_monotonic_ns: MetricValue::known(1_000_000_000),
            total_cpu_jiffies: 1,
            process_cpu: HashMap::new(),
            application_cgroup_cpu_usec: HashMap::from([(application_key.clone(), 100)]),
        };
        let resource = RawApplicationResourceRecord {
            application_key,
            process_count: 1,
            proc_cpu_jiffies_sum: MetricValue::known(1),
            rss_sum_bytes: MetricValue::known(1),
            pss_sum_bytes: MetricValue::known(1),
            fd_used_sum: MetricValue::known(1),
            fd_soft_limit_sum: MetricValue::known(10),
            fd_percent_of_attributed_sum: MetricValue::known(100.0),
            fd_percent_of_soft_limit_sum: MetricValue::known(10.0),
            cgroup_cpu_usage_usec: MetricValue::known(750_300),
            memory_current_bytes: MetricValue::known(1),
            cgroup_process_count: MetricValue::known(1),
        };
        let interval_ns = 1_500_400_000;

        let percent = cgroup_cpu_percent(
            Some(&resource),
            Some(&baseline),
            Some(interval_ns),
            1,
            "complete",
        );

        let expected = 750_200_f64 * 100.0 / 1_500_400_f64;
        assert!((percent.value.expect("known percent") - expected).abs() < f64::EPSILON);
    }
}
