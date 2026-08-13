use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::CapabilityAvailability;

pub const NETWORK_SCHEMA_VERSION: u16 = 1;
pub const MAX_NETWORK_INTERFACES: usize = 256;
pub const MAX_NETWORK_APPLICATIONS: usize = 1_024;
pub const MAX_INTERFACE_NAME_BYTES: usize = 256;
pub const NETWORK_TOTAL_SCOPE: &str = "inclusive_interfaces";

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkFreshness {
    Fresh,
    WarmingUp,
    Stale,
    Unknown,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkInterfaceKind {
    Physical,
    Loopback,
    Tunnel,
    Virtual,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkRateState {
    Known,
    WarmingUp,
    SamplingGap,
    CounterResetOrWrap,
    CountersUnavailable,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkInterfaceTransition {
    Stable,
    FirstObservation,
    HotplugAdded,
    SamplingGap,
    CounterResetOrWrap,
    CountersUnavailable,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkLayeredAccounting {
    NotDetected,
    PossibleVpnUnderlayDoubleCounting,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkCapabilityState {
    pub status: CapabilityAvailability,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkByteTotals {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkTrafficTotals {
    pub scope: String,
    /// Inclusive sum of all reported interfaces. VPN underlay traffic may be counted twice.
    pub all_interfaces: NetworkByteTotals,
    pub physical: NetworkByteTotals,
    pub loopback: NetworkByteTotals,
    pub tunnel: NetworkByteTotals,
    pub other_virtual: NetworkByteTotals,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkRate {
    pub rx_bytes_per_second: Option<f64>,
    pub tx_bytes_per_second: Option<f64>,
    pub state: NetworkRateState,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkInterfaceSample {
    pub index: u32,
    pub name: String,
    pub kind: NetworkInterfaceKind,
    pub kernel_kind: Option<String>,
    pub is_up: bool,
    pub carrier_up: bool,
    pub counters: Option<NetworkByteTotals>,
    pub rate: NetworkRate,
    pub transition: NetworkInterfaceTransition,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkApplicationTraffic {
    pub application_key: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_share_percent: Option<f64>,
    pub tx_share_percent: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkCoverage {
    pub reported_interfaces: u32,
    pub interfaces_with_counters: u32,
    pub includes_loopback: bool,
    pub includes_tunnels: bool,
    pub layered_accounting: NetworkLayeredAccounting,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSnapshot {
    pub schema_version: u16,
    pub snapshot_id: Uuid,
    pub captured_at_unix_ms: Option<i64>,
    pub observed_boottime_ms: Option<u64>,
    pub sample_interval_ms: Option<u64>,
    pub last_success_at_unix_ms: Option<i64>,
    pub freshness: NetworkFreshness,
    pub retryable: bool,
    pub system_traffic: NetworkCapabilityState,
    pub per_application: NetworkCapabilityState,
    pub coverage: NetworkCoverage,
    pub totals: Option<NetworkTrafficTotals>,
    pub aggregate_rate: NetworkRate,
    pub interfaces: Vec<NetworkInterfaceSample>,
    pub applications: Vec<NetworkApplicationTraffic>,
}

impl NetworkSnapshot {
    pub fn unavailable(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            schema_version: NETWORK_SCHEMA_VERSION,
            snapshot_id: Uuid::new_v4(),
            captured_at_unix_ms: None,
            observed_boottime_ms: None,
            sample_interval_ms: None,
            last_success_at_unix_ms: None,
            freshness: NetworkFreshness::Unknown,
            retryable: true,
            system_traffic: NetworkCapabilityState {
                status: CapabilityAvailability::Unreachable,
                reason: reason.clone(),
            },
            per_application: NetworkCapabilityState {
                status: CapabilityAvailability::Unsupported,
                reason: "per_app_collector_unavailable".to_owned(),
            },
            coverage: NetworkCoverage {
                reported_interfaces: 0,
                interfaces_with_counters: 0,
                includes_loopback: false,
                includes_tunnels: false,
                layered_accounting: NetworkLayeredAccounting::NotDetected,
                reason,
            },
            totals: None,
            aggregate_rate: NetworkRate {
                rx_bytes_per_second: None,
                tx_bytes_per_second: None,
                state: NetworkRateState::CountersUnavailable,
                reason: "network_snapshot_unavailable".to_owned(),
            },
            interfaces: Vec::new(),
            applications: Vec::new(),
        }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != NETWORK_SCHEMA_VERSION {
            return Err("network_schema_must_be_1");
        }
        if self.interfaces.len() > MAX_NETWORK_INTERFACES {
            return Err("network_interfaces_exceeds_256");
        }
        if self.applications.len() > MAX_NETWORK_APPLICATIONS {
            return Err("network_applications_exceeds_1024");
        }
        if self.coverage.reported_interfaces as usize != self.interfaces.len() {
            return Err("network_reported_interface_count_mismatch");
        }
        if self.coverage.interfaces_with_counters > self.coverage.reported_interfaces {
            return Err("network_counter_coverage_invalid");
        }
        if self.interfaces.iter().any(|interface| {
            interface.name.is_empty()
                || interface.name.len() > MAX_INTERFACE_NAME_BYTES
                || interface.name.contains('\0')
        }) {
            return Err("network_interface_name_invalid");
        }
        if self.applications.iter().any(|application| {
            application.application_key.is_empty()
                || application.application_key.len() > 512
                || application.application_key.contains('\0')
                || !valid_percent(application.rx_share_percent)
                || !valid_percent(application.tx_share_percent)
        }) {
            return Err("network_application_record_invalid");
        }
        if !valid_rate(&self.aggregate_rate)
            || self
                .interfaces
                .iter()
                .any(|interface| !valid_rate(&interface.rate))
        {
            return Err("network_rate_invalid");
        }
        if self
            .totals
            .as_ref()
            .is_some_and(|totals| totals.scope != NETWORK_TOTAL_SCOPE)
        {
            return Err("network_total_scope_invalid");
        }
        if self.system_traffic.status == CapabilityAvailability::Healthy
            && (self.captured_at_unix_ms.is_none()
                || self.observed_boottime_ms.is_none()
                || self.last_success_at_unix_ms.is_none()
                || self.totals.is_none()
                || self.freshness != NetworkFreshness::Fresh)
        {
            return Err("network_healthy_state_missing_facts");
        }
        if self.per_application.status == CapabilityAvailability::Unsupported
            && !self.applications.is_empty()
        {
            return Err("network_unsupported_per_app_has_records");
        }
        Ok(())
    }
}

fn valid_percent(value: Option<f64>) -> bool {
    value.is_none_or(|value| value.is_finite() && (0.0..=100.0).contains(&value))
}

fn valid_rate(rate: &NetworkRate) -> bool {
    let values_are_valid = [rate.rx_bytes_per_second, rate.tx_bytes_per_second]
        .into_iter()
        .flatten()
        .all(|value| value.is_finite() && value >= 0.0);
    let known_shape = if rate.state == NetworkRateState::Known {
        rate.rx_bytes_per_second.is_some() && rate.tx_bytes_per_second.is_some()
    } else {
        rate.rx_bytes_per_second.is_none() && rate.tx_bytes_per_second.is_none()
    };
    values_are_valid && known_shape
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_snapshot_does_not_invent_zero_traffic() {
        let snapshot = NetworkSnapshot::unavailable("rtnetlink_unavailable");

        assert_eq!(snapshot.schema_version, NETWORK_SCHEMA_VERSION);
        assert_eq!(snapshot.totals, None);
        assert!(snapshot.interfaces.is_empty());
        assert_eq!(snapshot.validate(), Ok(()));
    }

    #[test]
    fn unsupported_per_app_cannot_carry_estimated_records() {
        let mut snapshot = NetworkSnapshot::unavailable("rtnetlink_unavailable");
        snapshot.applications.push(NetworkApplicationTraffic {
            application_key: "forged".to_owned(),
            rx_bytes: 1,
            tx_bytes: 1,
            rx_share_percent: Some(50.0),
            tx_share_percent: Some(50.0),
        });

        assert_eq!(
            snapshot.validate(),
            Err("network_unsupported_per_app_has_records")
        );
    }
}
