mod capability;
mod health;
mod network;
mod notes;
mod speedtest;
mod telemetry;
mod usage;

pub use capability::{
    APPD_HEALTH_CAPABILITY, Capability, CapabilityAvailability, CapabilityRuntime,
    CapabilityRuntimeState, KNOWN_CAPABILITIES, NETWORK_DEEPTEST_CAPABILITY,
    NETWORK_PER_APP_CAPABILITY, NETWORK_SPEEDTEST_CAPABILITY, NETWORK_SYSTEM_CAPABILITY,
    NOT_IMPLEMENTED_REASON, NOTES_CAPABILITY, REMOTE_FTP_CAPABILITY, REMOTE_SFTP_CAPABILITY,
    REMOTE_SMB_CAPABILITY, REMOTE_SSH_CAPABILITY, TELEMETRY_SNAPSHOT_CAPABILITY,
    TRANSFERS_CAPABILITY, USAGE_FOREGROUND_CAPABILITY, capability_catalog,
};
pub use health::{HealthState, RequestHealth, aggregate_request_health, health_reason};
pub use network::{
    MAX_INTERFACE_NAME_BYTES, MAX_NETWORK_APPLICATIONS, MAX_NETWORK_INTERFACES,
    NETWORK_SCHEMA_VERSION, NETWORK_TOTAL_SCOPE, NetworkApplicationTraffic, NetworkByteTotals,
    NetworkCapabilityState, NetworkCoverage, NetworkFreshness, NetworkInterfaceKind,
    NetworkInterfaceSample, NetworkInterfaceTransition, NetworkLayeredAccounting, NetworkRate,
    NetworkRateState, NetworkSnapshot, NetworkTrafficTotals,
};
pub use notes::{
    MAX_NOTE_BODY_BYTES, MAX_NOTE_CONTENT_BASE64_BYTES, MAX_NOTE_EXPORT_BYTES,
    MAX_NOTE_EXPORT_DATA_FRAMES, MAX_NOTE_QUERY_LIMIT, MAX_NOTE_QUERY_OFFSET,
    MAX_NOTE_SEARCH_CHARS, MAX_NOTE_STAGED_BYTES, MAX_NOTE_TAG_CHARS, MAX_NOTE_TAGS,
    MAX_NOTE_TITLE_CHARS, MAX_NOTE_UPLOAD_SESSIONS, NOTE_CONTENT_CHUNK_BYTES, NOTES_SCHEMA_VERSION,
    NoteDeletedFilter, NoteDocument, NoteDraftMeta, NoteExport, NoteExportFormat,
    NoteMutationResult, NotePage, NoteQuery, NoteSort, NoteStatus, NoteSummary, NoteWriteIntent,
    NotesCommand, NotesOutput, validate_sha256,
};
pub use speedtest::{
    SPEEDTEST_LATENCY_PROBES_PER_TARGET, SPEEDTEST_MAX_BANDWIDTH_MEASUREMENTS,
    SPEEDTEST_MAX_LATENCY_TARGETS, SPEEDTEST_MAX_MIRRORS, SPEEDTEST_MAX_REASON_BYTES,
    SPEEDTEST_SCHEMA_VERSION, BandwidthKind, BandwidthMeasurement, Iperf3Direction, Iperf3Result,
    IpPurityResult, LatencyProbe, LatencyTargetResult, LinssidLaunchResult, SpeedTestBasicEnd,
    SpeedTestCancelResult, SpeedTestDeepCommand, SpeedTestDeepOutput, SpeedTestStage,
    SpeedTestStageData, WifiNetwork, WifiScanResult,
};
pub use telemetry::{
    ApplicationSample, GroupingResolution, IssueCount, MetricState, MetricValue, SystemFdSample,
    TELEMETRY_SCHEMA_VERSION, TELEMETRY_SCOPE_FULL_CGROUP, TELEMETRY_SCOPE_SAME_EUID,
    TELEMETRY_SCOPE_SYSTEM, TelemetryFreshness, TelemetrySnapshot, TelemetryStatus,
};
pub use usage::{
    MAX_USAGE_APP_ID_BYTES, MAX_USAGE_APPLICATIONS, MAX_USAGE_BUCKET_KEY_BYTES, USAGE_DEFINITION,
    USAGE_SCHEMA_VERSION, UsageApplicationDuration, UsageCoverage, UsagePeriod, UsageSummary,
    UsageSummaryQuery,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_reflects_runtime_and_unimplemented_capabilities() {
        let runtime = CapabilityRuntime::new(
            CapabilityRuntimeState::healthy("appd_online"),
            CapabilityRuntimeState::degraded("telemetry_warming_up"),
            CapabilityRuntimeState::healthy("rtnetlink_system_counters_available"),
            CapabilityRuntimeState::unsupported("unprivileged_bpf_permanently_disabled"),
            CapabilityRuntimeState::degraded("usage_warming_up"),
        );
        let capabilities = capability_catalog(&runtime);

        assert_eq!(capabilities.len(), KNOWN_CAPABILITIES.len());
        assert_eq!(
            capabilities
                .iter()
                .filter(|capability| capability.status == CapabilityAvailability::Healthy)
                .count(),
            2
        );
        assert_eq!(capabilities[0].id, APPD_HEALTH_CAPABILITY);
        assert_eq!(capabilities[1].id, TELEMETRY_SNAPSHOT_CAPABILITY);
        assert_eq!(capabilities[1].status, CapabilityAvailability::Degraded);
        assert_eq!(capabilities[2].status, CapabilityAvailability::Healthy);
        assert_eq!(capabilities[3].status, CapabilityAvailability::Unsupported);
        assert_eq!(capabilities[4].id, NETWORK_SPEEDTEST_CAPABILITY);
        assert_eq!(capabilities[4].status, CapabilityAvailability::Unsupported);
        assert_eq!(capabilities[5].id, NETWORK_DEEPTEST_CAPABILITY);
        assert_eq!(capabilities[6].status, CapabilityAvailability::Degraded);
    }

    #[test]
    fn health_reason_preserves_top_level_semantics() {
        assert_eq!(
            health_reason(HealthState::Healthy),
            "all_requested_capabilities_available"
        );
        assert_eq!(
            health_reason(HealthState::Degraded),
            "appd_online_with_unavailable_capabilities"
        );
    }

    #[test]
    fn public_domain_has_current_schema_and_no_process_records() {
        let snapshot = TelemetrySnapshot::unavailable("collector_unavailable");

        assert_eq!(snapshot.schema_version, TELEMETRY_SCHEMA_VERSION);
        assert_eq!(snapshot.freshness, TelemetryFreshness::Unknown);
        assert!(snapshot.applications.is_empty());
    }
}
