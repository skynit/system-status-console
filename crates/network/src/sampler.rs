use crate::{
    CapabilityState, InterfaceId, InterfaceKind, InterfaceTraffic, InterfaceTransition,
    LayeredAccounting, LinkCounters, NetworkCoverage, NetworkEvent, NetworkSnapshot, RateState,
    RawInterface, TrafficRate, TrafficTotals,
};
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

#[derive(Debug, Clone, Copy)]
pub struct SamplerConfig {
    pub maximum_rate_interval: Duration,
}

impl Default for SamplerConfig {
    fn default() -> Self {
        Self {
            maximum_rate_interval: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct BaselineCounters {
    observed_boottime: Duration,
    counters: LinkCounters,
}

pub struct NetworkSampler {
    config: SamplerConfig,
    baselines: HashMap<InterfaceId, BaselineCounters>,
    names_by_index: HashMap<u32, String>,
    previous_ids: HashSet<InterfaceId>,
    observed_once: bool,
    previous_observed_boottime: Option<Duration>,
}

impl NetworkSampler {
    pub fn new(config: SamplerConfig) -> Self {
        Self {
            config,
            baselines: HashMap::new(),
            names_by_index: HashMap::new(),
            previous_ids: HashSet::new(),
            observed_once: false,
            previous_observed_boottime: None,
        }
    }

    pub fn reset(&mut self) {
        self.baselines.clear();
        self.names_by_index.clear();
        self.previous_ids.clear();
        self.observed_once = false;
        self.previous_observed_boottime = None;
    }

    pub fn observe(
        &mut self,
        mut raw_interfaces: Vec<RawInterface>,
        observed_boottime: Duration,
    ) -> NetworkSnapshot {
        raw_interfaces.sort_by_key(|interface| interface.id.index);
        let sampling_gap = self.previous_observed_boottime.is_some_and(|previous| {
            observed_boottime
                .checked_sub(previous)
                .is_none_or(|elapsed| elapsed > self.config.maximum_rate_interval)
        });
        let current_ids = raw_interfaces
            .iter()
            .map(|interface| interface.id.clone())
            .collect::<HashSet<_>>();
        let mut events = Vec::new();
        if sampling_gap {
            events.push(NetworkEvent::SamplingGap);
        }
        if self.observed_once {
            for previous in &self.previous_ids {
                if !current_ids.contains(previous)
                    && !current_ids
                        .iter()
                        .any(|current| current.index == previous.index)
                {
                    events.push(NetworkEvent::InterfaceRemoved(previous.clone()));
                }
            }
        }

        let mut next_baselines = HashMap::new();
        let mut next_names = HashMap::new();
        let mut interfaces = Vec::with_capacity(raw_interfaces.len());
        for interface in raw_interfaces {
            if let Some(previous_name) = self.names_by_index.get(&interface.id.index)
                && previous_name != &interface.id.name
            {
                events.push(NetworkEvent::InterfaceRenamed {
                    index: interface.id.index,
                    previous_name: previous_name.clone(),
                    current_name: interface.id.name.clone(),
                });
            }
            let previous = self.baselines.get(&interface.id).copied();
            let (rate, transition) = interface_rate(
                &interface,
                previous,
                observed_boottime,
                self.observed_once,
                sampling_gap,
            );
            if self.observed_once
                && !self.previous_ids.contains(&interface.id)
                && !self
                    .previous_ids
                    .iter()
                    .any(|previous| previous.index == interface.id.index)
            {
                events.push(NetworkEvent::InterfaceAdded(interface.id.clone()));
            }
            if transition == InterfaceTransition::CounterResetOrWrap {
                events.push(NetworkEvent::CounterResetOrWrap(interface.id.clone()));
            }
            if let Some(counters) = interface.counters {
                next_baselines.insert(
                    interface.id.clone(),
                    BaselineCounters {
                        observed_boottime,
                        counters,
                    },
                );
            }
            next_names.insert(interface.id.index, interface.id.name.clone());
            interfaces.push(InterfaceTraffic {
                interface,
                rate,
                transition,
            });
        }

        let (totals, coverage, system_traffic) = totals_and_coverage(&interfaces);
        let aggregate_rate = aggregate_rate(&interfaces, sampling_gap);
        self.baselines = next_baselines;
        self.names_by_index = next_names;
        self.previous_ids = current_ids;
        self.observed_once = true;
        self.previous_observed_boottime = Some(observed_boottime);

        NetworkSnapshot {
            observed_boottime,
            system_traffic,
            per_application: CapabilityState::unsupported("per_app_collector_not_probed"),
            coverage,
            totals,
            aggregate_rate,
            interfaces,
            applications: Vec::new(),
            events,
        }
    }
}

fn interface_rate(
    interface: &RawInterface,
    previous: Option<BaselineCounters>,
    observed_boottime: Duration,
    observed_once: bool,
    sampling_gap: bool,
) -> (TrafficRate, InterfaceTransition) {
    let Some(current) = interface.counters else {
        return (
            TrafficRate::unavailable(RateState::CountersUnavailable, "interface_counters_missing"),
            InterfaceTransition::CountersUnavailable,
        );
    };
    let Some(previous) = previous else {
        let transition = if observed_once {
            InterfaceTransition::HotplugAdded
        } else {
            InterfaceTransition::FirstObservation
        };
        return (
            TrafficRate::unavailable(RateState::WarmingUp, "interface_baseline_missing"),
            transition,
        );
    };
    if sampling_gap {
        return (
            TrafficRate::unavailable(RateState::SamplingGap, "sampling_or_suspend_gap"),
            InterfaceTransition::SamplingGap,
        );
    }
    let Some(elapsed) = observed_boottime.checked_sub(previous.observed_boottime) else {
        return (
            TrafficRate::unavailable(RateState::SamplingGap, "boottime_not_monotonic"),
            InterfaceTransition::SamplingGap,
        );
    };
    let seconds = elapsed.as_secs_f64();
    if seconds <= 0.0 {
        return (
            TrafficRate::unavailable(RateState::SamplingGap, "sample_interval_zero"),
            InterfaceTransition::SamplingGap,
        );
    }
    if current.width != previous.counters.width
        || current.rx_bytes < previous.counters.rx_bytes
        || current.tx_bytes < previous.counters.tx_bytes
    {
        // A single rtnetlink sequence cannot distinguish a driver reset from a
        // native-width wrap. Both invalidate this interval instead of inventing a delta.
        return (
            TrafficRate::unavailable(
                RateState::CounterResetOrWrap,
                "counter_reset_or_native_width_wrap",
            ),
            InterfaceTransition::CounterResetOrWrap,
        );
    }
    (
        TrafficRate {
            rx_bytes_per_second: Some(
                (current.rx_bytes - previous.counters.rx_bytes) as f64 / seconds,
            ),
            tx_bytes_per_second: Some(
                (current.tx_bytes - previous.counters.tx_bytes) as f64 / seconds,
            ),
            state: RateState::Known,
            reason: "rate_from_monotonic_counter_delta",
        },
        InterfaceTransition::Stable,
    )
}

fn totals_and_coverage(
    interfaces: &[InterfaceTraffic],
) -> (TrafficTotals, NetworkCoverage, CapabilityState) {
    let mut totals = TrafficTotals::default();
    let mut with_counters = 0;
    let mut physical_present = false;
    let mut loopback_present = false;
    let mut tunnel_present = false;
    for interface in interfaces {
        match interface.interface.kind {
            InterfaceKind::Physical => physical_present = true,
            InterfaceKind::Loopback => loopback_present = true,
            InterfaceKind::Tunnel => tunnel_present = true,
            InterfaceKind::Virtual => {}
        }
        let Some(counters) = interface.interface.counters else {
            continue;
        };
        with_counters += 1;
        totals.all_interfaces.add(counters);
        match interface.interface.kind {
            InterfaceKind::Physical => totals.physical.add(counters),
            InterfaceKind::Loopback => totals.loopback.add(counters),
            InterfaceKind::Tunnel => totals.tunnel.add(counters),
            InterfaceKind::Virtual => totals.other_virtual.add(counters),
        }
    }
    let complete = with_counters == interfaces.len();
    let layered_accounting = if physical_present && tunnel_present {
        LayeredAccounting::PossibleVpnUnderlayDoubleCounting
    } else {
        LayeredAccounting::NotDetected
    };
    (
        totals,
        NetworkCoverage {
            reported_interfaces: interfaces.len(),
            interfaces_with_counters: with_counters,
            includes_loopback: loopback_present,
            includes_tunnels: tunnel_present,
            layered_accounting,
            reason: if complete {
                "all_reported_interfaces_have_counters"
            } else {
                "some_interface_counters_missing"
            },
        },
        if interfaces.is_empty() {
            CapabilityState::degraded("rtnetlink_no_interfaces_reported")
        } else if complete {
            CapabilityState::healthy("rtnetlink_system_counters_available")
        } else {
            CapabilityState::degraded("rtnetlink_interface_coverage_partial")
        },
    )
}

fn aggregate_rate(interfaces: &[InterfaceTraffic], sampling_gap: bool) -> TrafficRate {
    if sampling_gap {
        return TrafficRate::unavailable(RateState::SamplingGap, "sampling_or_suspend_gap");
    }
    if interfaces.is_empty() {
        return TrafficRate::unavailable(RateState::CountersUnavailable, "no_interfaces_reported");
    }
    if let Some(state) = interfaces
        .iter()
        .map(|interface| interface.rate.state)
        .find(|state| *state != RateState::Known)
    {
        let reason = match state {
            RateState::CounterResetOrWrap => "aggregate_counter_reset_or_wrap",
            RateState::SamplingGap => "aggregate_sampling_gap",
            RateState::CountersUnavailable => "aggregate_counters_unavailable",
            RateState::WarmingUp => "aggregate_rate_warming_up",
            RateState::Known => unreachable!(),
        };
        return TrafficRate::unavailable(state, reason);
    }
    TrafficRate {
        rx_bytes_per_second: Some(
            interfaces
                .iter()
                .filter_map(|interface| interface.rate.rx_bytes_per_second)
                .sum(),
        ),
        tx_bytes_per_second: Some(
            interfaces
                .iter()
                .filter_map(|interface| interface.rate.tx_bytes_per_second)
                .sum(),
        ),
        state: RateState::Known,
        reason: "sum_of_complete_interface_rates_may_include_layered_traffic",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CounterWidth, LayeredAccounting};

    fn interface(index: u32, name: &str, kind: InterfaceKind, rx: u64, tx: u64) -> RawInterface {
        RawInterface {
            id: InterfaceId {
                index,
                name: name.to_owned(),
            },
            kind,
            kernel_kind: (kind == InterfaceKind::Tunnel).then(|| "tun".to_owned()),
            is_up: true,
            carrier_up: true,
            counters: Some(LinkCounters {
                rx_bytes: rx,
                tx_bytes: tx,
                width: CounterWidth::Bits64,
            }),
        }
    }

    #[test]
    fn rates_warm_then_use_boottime_deltas() {
        let mut sampler = NetworkSampler::new(SamplerConfig::default());
        let first = sampler.observe(
            vec![interface(2, "eth0", InterfaceKind::Physical, 100, 200)],
            Duration::from_secs(10),
        );
        assert_eq!(first.interfaces[0].rate.state, RateState::WarmingUp);
        assert_eq!(first.totals.physical.rx_bytes, 100);

        let second = sampler.observe(
            vec![interface(2, "eth0", InterfaceKind::Physical, 300, 500)],
            Duration::from_secs(12),
        );
        assert_eq!(second.interfaces[0].rate.rx_bytes_per_second, Some(100.0));
        assert_eq!(second.interfaces[0].rate.tx_bytes_per_second, Some(150.0));
        assert_eq!(second.aggregate_rate.state, RateState::Known);
    }

    #[test]
    fn reset_or_wrap_never_becomes_a_huge_rate() {
        let mut sampler = NetworkSampler::new(SamplerConfig::default());
        sampler.observe(
            vec![interface(2, "eth0", InterfaceKind::Physical, 1_000, 2_000)],
            Duration::from_secs(10),
        );
        let reset = sampler.observe(
            vec![interface(2, "eth0", InterfaceKind::Physical, 10, 20)],
            Duration::from_secs(11),
        );
        assert_eq!(
            reset.interfaces[0].rate.state,
            RateState::CounterResetOrWrap
        );
        assert_eq!(reset.interfaces[0].rate.rx_bytes_per_second, None);
        assert!(matches!(
            reset.events[0],
            NetworkEvent::CounterResetOrWrap(_)
        ));
    }

    #[test]
    fn hotplug_and_suspend_gap_invalidate_aggregate_rate() {
        let mut sampler = NetworkSampler::new(SamplerConfig::default());
        sampler.observe(
            vec![interface(2, "eth0", InterfaceKind::Physical, 100, 100)],
            Duration::from_secs(10),
        );
        let added = sampler.observe(
            vec![
                interface(2, "eth0", InterfaceKind::Physical, 200, 200),
                interface(3, "wg0", InterfaceKind::Tunnel, 50, 50),
            ],
            Duration::from_secs(11),
        );
        assert_eq!(
            added.interfaces[1].transition,
            InterfaceTransition::HotplugAdded
        );
        assert_eq!(added.aggregate_rate.rx_bytes_per_second, None);
        assert_eq!(
            added.coverage.layered_accounting,
            LayeredAccounting::PossibleVpnUnderlayDoubleCounting
        );

        let resumed = sampler.observe(
            vec![
                interface(2, "eth0", InterfaceKind::Physical, 500, 500),
                interface(3, "wg0", InterfaceKind::Tunnel, 200, 200),
            ],
            Duration::from_secs(30),
        );
        assert_eq!(resumed.interfaces[0].rate.state, RateState::SamplingGap);
        assert!(resumed.events.contains(&NetworkEvent::SamplingGap));
    }

    #[test]
    fn removed_and_renamed_interfaces_are_reported() {
        let mut sampler = NetworkSampler::new(SamplerConfig::default());
        sampler.observe(
            vec![
                interface(2, "eth0", InterfaceKind::Physical, 100, 100),
                interface(3, "old", InterfaceKind::Virtual, 100, 100),
            ],
            Duration::from_secs(1),
        );
        let snapshot = sampler.observe(
            vec![interface(3, "new", InterfaceKind::Virtual, 200, 200)],
            Duration::from_secs(2),
        );
        assert!(snapshot.events.iter().any(|event| matches!(
            event,
            NetworkEvent::InterfaceRemoved(id) if id.index == 2
        )));
        assert!(
            snapshot
                .events
                .iter()
                .any(|event| matches!(event, NetworkEvent::InterfaceRenamed { index: 3, .. }))
        );
        assert!(!snapshot.events.iter().any(|event| matches!(
            event,
            NetworkEvent::InterfaceAdded(id) | NetworkEvent::InterfaceRemoved(id)
                if id.index == 3
        )));
    }

    #[test]
    fn empty_dump_and_missing_counters_are_degraded() {
        let mut sampler = NetworkSampler::new(SamplerConfig::default());
        let empty = sampler.observe(Vec::new(), Duration::from_secs(1));
        assert_eq!(
            empty.system_traffic,
            CapabilityState::degraded("rtnetlink_no_interfaces_reported")
        );

        let mut no_counters = interface(2, "eth0", InterfaceKind::Physical, 0, 0);
        no_counters.counters = None;
        let partial = sampler.observe(vec![no_counters], Duration::from_secs(2));
        assert_eq!(
            partial.system_traffic,
            CapabilityState::degraded("rtnetlink_interface_coverage_partial")
        );
        assert_eq!(partial.aggregate_rate.state, RateState::CountersUnavailable);
    }
}
