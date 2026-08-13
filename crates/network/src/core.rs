use crate::CapabilityState;
use std::{
    collections::{BTreeMap, HashSet},
    fmt, fs,
    path::PathBuf,
};

pub const MAX_CGROUP_TRAFFIC_RECORDS: usize = 4_096;
pub const MAX_APPLICATION_TRAFFIC_RECORDS: usize = 1_024;
const MAX_APPLICATION_KEY_BYTES: usize = 512;
const CAP_NET_ADMIN: u32 = 12;
const CAP_BPF: u32 = 39;

#[derive(Debug, Clone)]
pub struct CoreProbePaths {
    pub unprivileged_bpf_disabled: PathBuf,
    pub kernel_btf: PathBuf,
    pub libbpf_candidates: Vec<PathBuf>,
}

impl Default for CoreProbePaths {
    fn default() -> Self {
        Self {
            unprivileged_bpf_disabled: PathBuf::from("/proc/sys/kernel/unprivileged_bpf_disabled"),
            kernel_btf: PathBuf::from("/sys/kernel/btf/vmlinux"),
            libbpf_candidates: vec![
                PathBuf::from("/usr/lib/libbpf.so.1"),
                PathBuf::from("/usr/lib64/libbpf.so.1"),
                PathBuf::from("/usr/lib/x86_64-linux-gnu/libbpf.so.1"),
                PathBuf::from("/usr/lib/aarch64-linux-gnu/libbpf.so.1"),
                PathBuf::from("/lib/libbpf.so.1"),
                PathBuf::from("/lib64/libbpf.so.1"),
                PathBuf::from("/lib/x86_64-linux-gnu/libbpf.so.1"),
                PathBuf::from("/lib/aarch64-linux-gnu/libbpf.so.1"),
            ],
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct CoreProbeFacts {
    pub effective_uid: u32,
    pub effective_capabilities: Option<u64>,
    pub unprivileged_bpf_disabled: Option<u32>,
    pub kernel_btf_available: bool,
    pub libbpf_available: bool,
    /// True only for the dedicated helper build that embeds the CO-RE object.
    pub collector_built: bool,
    /// Set only after the object loads and both cgroup hooks attach successfully.
    pub collector_attached: bool,
}

pub fn probe_core_support(paths: &CoreProbePaths) -> CapabilityState {
    probe_core_support_with_collector(paths, false, false)
}

pub fn probe_core_support_with_collector(
    paths: &CoreProbePaths,
    collector_built: bool,
    collector_attached: bool,
) -> CapabilityState {
    let setting = fs::read_to_string(&paths.unprivileged_bpf_disabled)
        .ok()
        .and_then(|value| value.trim().parse().ok());
    // SAFETY: geteuid has no pointer arguments and no failure mode.
    let effective_uid = unsafe { libc::geteuid() };
    assess_core_support(CoreProbeFacts {
        effective_uid,
        effective_capabilities: effective_capabilities(),
        unprivileged_bpf_disabled: setting,
        kernel_btf_available: paths.kernel_btf.is_file(),
        libbpf_available: paths.libbpf_candidates.iter().any(|path| path.exists()),
        collector_built,
        collector_attached,
    })
}

pub fn assess_core_support(facts: CoreProbeFacts) -> CapabilityState {
    let privileged = facts.effective_uid == 0
        || facts.effective_capabilities.is_some_and(|capabilities| {
            capabilities & (1_u64 << CAP_BPF) != 0 && capabilities & (1_u64 << CAP_NET_ADMIN) != 0
        });
    if !privileged && facts.unprivileged_bpf_disabled == Some(2) {
        return CapabilityState::unsupported("unprivileged_bpf_permanently_disabled");
    }
    if !facts.kernel_btf_available {
        return CapabilityState::unsupported("kernel_btf_unavailable");
    }
    if !facts.libbpf_available {
        return CapabilityState::unsupported("libbpf_runtime_unavailable");
    }
    if !facts.collector_built {
        return CapabilityState::unsupported("core_cgroup_collector_not_built");
    }
    if !facts.collector_attached {
        return CapabilityState::degraded("core_cgroup_collector_not_attached");
    }
    CapabilityState::healthy("core_cgroup_collector_attached")
}

fn effective_capabilities() -> Option<u64> {
    fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("CapEff:\t"))
        .and_then(|value| u64::from_str_radix(value.trim(), 16).ok())
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CgroupTraffic {
    pub cgroup_id: u64,
    pub application_key: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ApplicationTraffic {
    pub application_key: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

#[derive(Debug)]
pub struct PerAppCollectorError {
    pub reason: &'static str,
}

impl PerAppCollectorError {
    pub const fn new(reason: &'static str) -> Self {
        Self { reason }
    }
}

impl fmt::Display for PerAppCollectorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.reason)
    }
}

impl std::error::Error for PerAppCollectorError {}

/// Boundary for the optional privileged collector. Implementations must attach
/// CO-RE programs to cgroup ingress/egress and key exact counters by cgroup ID.
/// They must never estimate per-application traffic from interface totals.
pub trait PerAppCollector: Send {
    fn capability(&self) -> CapabilityState;
    fn collect(&mut self) -> Result<Vec<CgroupTraffic>, PerAppCollectorError>;
}

pub(crate) fn aggregate_cgroup_traffic(
    records: Vec<CgroupTraffic>,
) -> Result<Vec<ApplicationTraffic>, PerAppCollectorError> {
    if records.len() > MAX_CGROUP_TRAFFIC_RECORDS {
        return Err(PerAppCollectorError::new(
            "per_app_cgroup_record_limit_exceeded",
        ));
    }
    let mut seen_cgroups = HashSet::with_capacity(records.len());
    let mut applications = BTreeMap::<String, (u64, u64)>::new();
    for record in records {
        if record.cgroup_id == 0 {
            return Err(PerAppCollectorError::new("per_app_cgroup_id_invalid"));
        }
        if !seen_cgroups.insert(record.cgroup_id) {
            return Err(PerAppCollectorError::new("per_app_cgroup_id_duplicate"));
        }
        if record.application_key.is_empty()
            || record.application_key.len() > MAX_APPLICATION_KEY_BYTES
            || record.application_key.contains('\0')
        {
            return Err(PerAppCollectorError::new("per_app_application_key_invalid"));
        }
        let totals = applications.entry(record.application_key).or_insert((0, 0));
        totals.0 = totals
            .0
            .checked_add(record.rx_bytes)
            .ok_or_else(|| PerAppCollectorError::new("per_app_counter_overflow"))?;
        totals.1 = totals
            .1
            .checked_add(record.tx_bytes)
            .ok_or_else(|| PerAppCollectorError::new("per_app_counter_overflow"))?;
    }
    if applications.len() > MAX_APPLICATION_TRAFFIC_RECORDS {
        return Err(PerAppCollectorError::new(
            "per_app_application_record_limit_exceeded",
        ));
    }
    Ok(applications
        .into_iter()
        .map(
            |(application_key, (rx_bytes, tx_bytes))| ApplicationTraffic {
                application_key,
                rx_bytes,
                tx_bytes,
            },
        )
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CapabilityStatus;

    fn supported_host() -> CoreProbeFacts {
        CoreProbeFacts {
            effective_uid: 0,
            effective_capabilities: Some(0),
            unprivileged_bpf_disabled: Some(2),
            kernel_btf_available: true,
            libbpf_available: true,
            collector_built: true,
            collector_attached: true,
        }
    }

    #[test]
    fn permanent_unprivileged_disable_is_explicitly_unsupported() {
        let mut facts = supported_host();
        facts.effective_uid = 1000;
        assert_eq!(
            assess_core_support(facts),
            CapabilityState {
                status: CapabilityStatus::Unsupported,
                reason: "unprivileged_bpf_permanently_disabled",
            }
        );
    }

    #[test]
    fn minimum_effective_capabilities_pass_the_unprivileged_gate() {
        let mut facts = supported_host();
        facts.effective_uid = 1000;
        facts.effective_capabilities = Some((1_u64 << CAP_BPF) | (1_u64 << CAP_NET_ADMIN));
        assert_eq!(
            assess_core_support(facts),
            CapabilityState::healthy("core_cgroup_collector_attached")
        );
    }

    #[test]
    fn either_capability_alone_still_fails_closed() {
        for capabilities in [None, Some(1_u64 << CAP_BPF), Some(1_u64 << CAP_NET_ADMIN)] {
            let mut facts = supported_host();
            facts.effective_uid = 1000;
            facts.effective_capabilities = capabilities;
            assert_eq!(
                assess_core_support(facts),
                CapabilityState::unsupported("unprivileged_bpf_permanently_disabled")
            );
        }
    }

    #[test]
    fn prerequisites_do_not_claim_collection_before_attach() {
        let mut facts = supported_host();
        facts.collector_attached = false;
        assert_eq!(
            assess_core_support(facts),
            CapabilityState::degraded("core_cgroup_collector_not_attached")
        );
    }

    #[test]
    fn cgroup_counters_are_aggregated_by_resolved_application_key() {
        let applications = aggregate_cgroup_traffic(vec![
            CgroupTraffic {
                cgroup_id: 10,
                application_key: "org.example.Editor.desktop".to_owned(),
                rx_bytes: 20,
                tx_bytes: 30,
            },
            CgroupTraffic {
                cgroup_id: 11,
                application_key: "org.example.Editor.desktop".to_owned(),
                rx_bytes: 5,
                tx_bytes: 7,
            },
            CgroupTraffic {
                cgroup_id: 12,
                application_key: "cgroup:opaque".to_owned(),
                rx_bytes: 2,
                tx_bytes: 3,
            },
        ])
        .expect("valid exact counters");

        assert_eq!(
            applications,
            vec![
                ApplicationTraffic {
                    application_key: "cgroup:opaque".to_owned(),
                    rx_bytes: 2,
                    tx_bytes: 3,
                },
                ApplicationTraffic {
                    application_key: "org.example.Editor.desktop".to_owned(),
                    rx_bytes: 25,
                    tx_bytes: 37,
                },
            ]
        );
    }

    #[test]
    fn duplicate_cgroup_or_counter_overflow_fails_closed() {
        let duplicate = aggregate_cgroup_traffic(vec![
            CgroupTraffic {
                cgroup_id: 10,
                application_key: "one".to_owned(),
                rx_bytes: 1,
                tx_bytes: 1,
            },
            CgroupTraffic {
                cgroup_id: 10,
                application_key: "two".to_owned(),
                rx_bytes: 1,
                tx_bytes: 1,
            },
        ])
        .expect_err("duplicate cgroup must not be counted twice");
        assert_eq!(duplicate.reason, "per_app_cgroup_id_duplicate");

        let overflow = aggregate_cgroup_traffic(vec![
            CgroupTraffic {
                cgroup_id: 10,
                application_key: "one".to_owned(),
                rx_bytes: u64::MAX,
                tx_bytes: 1,
            },
            CgroupTraffic {
                cgroup_id: 11,
                application_key: "one".to_owned(),
                rx_bytes: 1,
                tx_bytes: 1,
            },
        ])
        .expect_err("overflow must not saturate into a plausible counter");
        assert_eq!(overflow.reason, "per_app_counter_overflow");
    }
}
