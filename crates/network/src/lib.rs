#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

#[cfg(not(target_os = "linux"))]
compile_error!("localdesk-network requires Linux rtnetlink semantics");

mod core;
mod netlink;
mod sampler;

pub use core::{
    ApplicationTraffic, CgroupTraffic, CoreProbeFacts, CoreProbePaths,
    MAX_APPLICATION_TRAFFIC_RECORDS, MAX_CGROUP_TRAFFIC_RECORDS, PerAppCollector,
    PerAppCollectorError, assess_core_support, probe_core_support,
    probe_core_support_with_collector,
};
pub use netlink::{
    CollectError, DumpLimit, MAX_DUMP_BYTES, MAX_DUMP_DATAGRAM_BYTES, MAX_DUMP_DATAGRAMS,
    MAX_RAW_INTERFACES, RTNETLINK_RECEIVE_DEADLINE, RtnetlinkCollector, boottime_now,
};
pub use sampler::{NetworkSampler, SamplerConfig};

use std::{path::PathBuf, time::Duration};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum CapabilityStatus {
    Healthy,
    Degraded,
    Unsupported,
    Unreachable,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CapabilityState {
    pub status: CapabilityStatus,
    pub reason: &'static str,
}

impl CapabilityState {
    pub const fn healthy(reason: &'static str) -> Self {
        Self {
            status: CapabilityStatus::Healthy,
            reason,
        }
    }

    pub const fn degraded(reason: &'static str) -> Self {
        Self {
            status: CapabilityStatus::Degraded,
            reason,
        }
    }

    pub const fn unsupported(reason: &'static str) -> Self {
        Self {
            status: CapabilityStatus::Unsupported,
            reason,
        }
    }

    pub const fn unreachable(reason: &'static str) -> Self {
        Self {
            status: CapabilityStatus::Unreachable,
            reason,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum InterfaceKind {
    Physical,
    Loopback,
    Tunnel,
    Virtual,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum CounterWidth {
    Bits32,
    Bits64,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct LinkCounters {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub width: CounterWidth,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct InterfaceId {
    pub index: u32,
    pub name: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RawInterface {
    pub id: InterfaceId,
    pub kind: InterfaceKind,
    pub kernel_kind: Option<String>,
    pub is_up: bool,
    pub carrier_up: bool,
    pub counters: Option<LinkCounters>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum RateState {
    Known,
    WarmingUp,
    SamplingGap,
    CounterResetOrWrap,
    CountersUnavailable,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrafficRate {
    pub rx_bytes_per_second: Option<f64>,
    pub tx_bytes_per_second: Option<f64>,
    pub state: RateState,
    pub reason: &'static str,
}

impl TrafficRate {
    pub(crate) const fn unavailable(state: RateState, reason: &'static str) -> Self {
        Self {
            rx_bytes_per_second: None,
            tx_bytes_per_second: None,
            state,
            reason,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum InterfaceTransition {
    Stable,
    FirstObservation,
    HotplugAdded,
    SamplingGap,
    CounterResetOrWrap,
    CountersUnavailable,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InterfaceTraffic {
    pub interface: RawInterface,
    pub rate: TrafficRate,
    pub transition: InterfaceTransition,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum NetworkEvent {
    InterfaceAdded(InterfaceId),
    InterfaceRemoved(InterfaceId),
    InterfaceRenamed {
        index: u32,
        previous_name: String,
        current_name: String,
    },
    CounterResetOrWrap(InterfaceId),
    SamplingGap,
}

#[derive(Debug, Default, Clone, Copy, Eq, PartialEq)]
pub struct ByteTotals {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

impl ByteTotals {
    pub(crate) fn add(&mut self, counters: LinkCounters) {
        self.rx_bytes = self.rx_bytes.saturating_add(counters.rx_bytes);
        self.tx_bytes = self.tx_bytes.saturating_add(counters.tx_bytes);
    }
}

#[derive(Debug, Default, Clone, Eq, PartialEq)]
pub struct TrafficTotals {
    /// Inclusive sum of every reported interface. It is not unique host traffic.
    pub all_interfaces: ByteTotals,
    pub physical: ByteTotals,
    pub loopback: ByteTotals,
    pub tunnel: ByteTotals,
    pub other_virtual: ByteTotals,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum LayeredAccounting {
    NotDetected,
    PossibleVpnUnderlayDoubleCounting,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct NetworkCoverage {
    pub reported_interfaces: usize,
    pub interfaces_with_counters: usize,
    pub includes_loopback: bool,
    pub includes_tunnels: bool,
    pub layered_accounting: LayeredAccounting,
    pub reason: &'static str,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NetworkSnapshot {
    pub observed_boottime: Duration,
    pub system_traffic: CapabilityState,
    pub per_application: CapabilityState,
    pub coverage: NetworkCoverage,
    pub totals: TrafficTotals,
    pub aggregate_rate: TrafficRate,
    pub interfaces: Vec<InterfaceTraffic>,
    pub applications: Vec<ApplicationTraffic>,
    pub events: Vec<NetworkEvent>,
}

pub struct NetworkMonitor {
    collector: RtnetlinkCollector,
    sampler: NetworkSampler,
    core_probe_paths: CoreProbePaths,
    per_app_collector: Option<Box<dyn PerAppCollector>>,
}

impl Default for NetworkMonitor {
    fn default() -> Self {
        Self::new(SamplerConfig::default())
    }
}

impl NetworkMonitor {
    pub fn new(config: SamplerConfig) -> Self {
        Self {
            collector: RtnetlinkCollector::new(PathBuf::from("/sys/class/net")),
            sampler: NetworkSampler::new(config),
            core_probe_paths: CoreProbePaths::default(),
            per_app_collector: None,
        }
    }

    pub fn with_per_app_collector(mut self, collector: Box<dyn PerAppCollector>) -> Self {
        self.per_app_collector = Some(collector);
        self
    }

    pub fn collect(&mut self) -> Result<NetworkSnapshot, CollectError> {
        let observed_boottime = boottime_now()?;
        let interfaces = self.collector.collect()?;
        let mut snapshot = self.sampler.observe(interfaces, observed_boottime);
        collect_per_application(
            &mut snapshot,
            self.per_app_collector.as_deref_mut(),
            &self.core_probe_paths,
        );
        Ok(snapshot)
    }
}

fn collect_per_application<C>(
    snapshot: &mut NetworkSnapshot,
    collector: Option<&mut C>,
    probe_paths: &CoreProbePaths,
) where
    C: PerAppCollector + ?Sized,
{
    let Some(collector) = collector else {
        snapshot.per_application = probe_core_support(probe_paths);
        return;
    };
    let capability = collector.capability();
    if matches!(
        capability.status,
        CapabilityStatus::Unsupported | CapabilityStatus::Unreachable
    ) {
        snapshot.per_application = capability;
        snapshot.applications.clear();
        return;
    }
    match collector.collect().and_then(core::aggregate_cgroup_traffic) {
        Ok(applications) => {
            snapshot.per_application = capability;
            snapshot.applications = applications;
        }
        Err(error) => {
            snapshot.per_application = CapabilityState::degraded(error.reason);
            snapshot.applications.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixtureCollector {
        capability: CapabilityState,
        records: Option<Result<Vec<CgroupTraffic>, PerAppCollectorError>>,
    }

    impl PerAppCollector for FixtureCollector {
        fn capability(&self) -> CapabilityState {
            self.capability.clone()
        }

        fn collect(&mut self) -> Result<Vec<CgroupTraffic>, PerAppCollectorError> {
            self.records
                .take()
                .expect("collector must not be called more than once")
        }
    }

    fn snapshot() -> NetworkSnapshot {
        NetworkSampler::new(SamplerConfig::default()).observe(Vec::new(), Duration::from_secs(1))
    }

    #[test]
    fn monitor_path_consumes_exact_collector_records() {
        let mut snapshot = snapshot();
        let mut collector = FixtureCollector {
            capability: CapabilityState::healthy("core_cgroup_collector_attached"),
            records: Some(Ok(vec![CgroupTraffic {
                cgroup_id: 1,
                application_key: "editor.desktop".to_owned(),
                rx_bytes: 10,
                tx_bytes: 20,
            }])),
        };

        collect_per_application(
            &mut snapshot,
            Some(&mut collector),
            &CoreProbePaths::default(),
        );

        assert_eq!(
            snapshot.per_application,
            CapabilityState::healthy("core_cgroup_collector_attached")
        );
        assert_eq!(
            snapshot.applications,
            vec![ApplicationTraffic {
                application_key: "editor.desktop".to_owned(),
                rx_bytes: 10,
                tx_bytes: 20,
            }]
        );
    }

    #[test]
    fn unsupported_collector_is_not_invoked_and_publishes_no_records() {
        let mut snapshot = snapshot();
        let mut collector = FixtureCollector {
            capability: CapabilityState::unsupported("helper_permission_denied"),
            records: None,
        };

        collect_per_application(
            &mut snapshot,
            Some(&mut collector),
            &CoreProbePaths::default(),
        );

        assert_eq!(
            snapshot.per_application,
            CapabilityState::unsupported("helper_permission_denied")
        );
        assert!(snapshot.applications.is_empty());
    }
}
