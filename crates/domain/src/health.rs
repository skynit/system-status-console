use crate::capability::{Capability, CapabilityAvailability};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    Healthy,
    Degraded,
    Unsupported,
    Unreachable,
}

impl HealthState {
    pub const fn is_online(self) -> bool {
        matches!(self, Self::Healthy | Self::Degraded)
    }

    pub const fn is_healthy(self) -> bool {
        matches!(self, Self::Healthy)
    }
}

pub const fn health_reason(state: HealthState) -> &'static str {
    match state {
        HealthState::Healthy => "all_requested_capabilities_available",
        HealthState::Degraded => "appd_online_with_unavailable_capabilities",
        HealthState::Unsupported => "protocol_or_runtime_unsupported",
        HealthState::Unreachable => "appd_unreachable",
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct RequestHealth {
    pub health: HealthState,
    pub reason: String,
}

impl RequestHealth {
    pub fn new(health: HealthState, reason: impl Into<String>) -> Self {
        Self {
            health,
            reason: reason.into(),
        }
    }

    pub fn from_state(health: HealthState) -> Self {
        Self::new(health, health_reason(health))
    }
}

pub fn aggregate_request_health(
    daemon: Option<RequestHealth>,
    requested_capabilities: &[Capability],
) -> Option<RequestHealth> {
    let daemon = daemon?;

    if !daemon.health.is_online() {
        return Some(daemon);
    }

    let capability = requested_capabilities
        .iter()
        .find(|capability| capability.status != CapabilityAvailability::Healthy);
    match capability {
        Some(capability) => Some(RequestHealth::new(
            HealthState::Degraded,
            capability.reason.clone(),
        )),
        None => Some(daemon),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capability(status: CapabilityAvailability, reason: &str) -> Capability {
        Capability::new("test.capability", status, reason)
    }

    #[test]
    fn request_health_preserves_daemon_response_and_aggregates_requested_capabilities() {
        let daemon = RequestHealth::new(HealthState::Healthy, "daemon_online");
        let capabilities = [capability(
            CapabilityAvailability::Degraded,
            "telemetry_warming_up",
        )];

        assert_eq!(
            aggregate_request_health(Some(daemon), &capabilities),
            Some(RequestHealth::new(
                HealthState::Degraded,
                "telemetry_warming_up"
            ))
        );
    }

    #[test]
    fn missing_daemon_response_is_not_synthesized_as_unreachable() {
        assert_eq!(aggregate_request_health(None, &[]), None);
    }

    #[test]
    fn daemon_unsupported_or_unreachable_state_is_not_overwritten() {
        let unsupported = RequestHealth::new(HealthState::Unsupported, "protocol_unsupported");
        let unreachable = RequestHealth::new(HealthState::Unreachable, "daemon_unreachable");

        assert_eq!(
            aggregate_request_health(Some(unsupported.clone()), &[]),
            Some(unsupported)
        );
        assert_eq!(
            aggregate_request_health(Some(unreachable.clone()), &[]),
            Some(unreachable)
        );
    }
}
