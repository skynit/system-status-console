use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::CapabilityAvailability;

pub const USAGE_SCHEMA_VERSION: u16 = 3;
pub const MAX_USAGE_APPLICATIONS: usize = 1_024;
pub const MAX_USAGE_APP_ID_BYTES: usize = 512;
pub const MAX_USAGE_BUCKET_KEY_BYTES: usize = 10;
pub const USAGE_DEFINITION: &str = "foreground_unlocked_input_active_300s_monotonic";

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UsagePeriod {
    Daily,
    Weekly,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UsageSummaryQuery {
    pub period: UsagePeriod,
    pub bucket_key: String,
}

impl UsageSummaryQuery {
    pub fn validate(&self) -> Result<(), &'static str> {
        let bytes = self.bucket_key.as_bytes();
        if bytes.is_empty()
            || bytes.len() > MAX_USAGE_BUCKET_KEY_BYTES
            || self.bucket_key.contains('\0')
        {
            return Err("usage_bucket_key_invalid");
        }
        let valid = match self.period {
            UsagePeriod::Daily => valid_daily_key(bytes),
            UsagePeriod::Weekly => valid_weekly_key(bytes),
        };
        valid.then_some(()).ok_or("usage_bucket_key_invalid")
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UsageApplicationDuration {
    pub app_id: String,
    pub bucket_key: String,
    pub timezone_id: String,
    pub utc_offset_seconds: i32,
    pub duration_ns: u64,
    pub last_wall_utc_ms: i64,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UsageCoverage {
    pub status: CapabilityAvailability,
    pub reason: String,
    pub niri_event_stream_connected: bool,
    pub logind_session_available: bool,
    pub event_gap_count: u64,
    pub last_checkpoint_unix_ms: Option<i64>,
    pub tracking_started_unix_ms: Option<i64>,
    pub bucket_start_covered: bool,
    pub definition: String,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UsageSummary {
    pub schema_version: u16,
    pub snapshot_id: Uuid,
    pub captured_at_unix_ms: Option<i64>,
    pub query: UsageSummaryQuery,
    pub status: CapabilityAvailability,
    pub reason: String,
    pub retryable: bool,
    pub coverage: UsageCoverage,
    pub applications: Vec<UsageApplicationDuration>,
}

impl UsageSummary {
    pub fn unavailable(
        query: UsageSummaryQuery,
        reason: impl Into<String>,
        retryable: bool,
    ) -> Self {
        let reason = reason.into();
        Self {
            schema_version: USAGE_SCHEMA_VERSION,
            snapshot_id: Uuid::new_v4(),
            captured_at_unix_ms: None,
            query,
            status: CapabilityAvailability::Unreachable,
            reason: reason.clone(),
            retryable,
            coverage: UsageCoverage {
                status: CapabilityAvailability::Unreachable,
                reason,
                niri_event_stream_connected: false,
                logind_session_available: false,
                event_gap_count: 0,
                last_checkpoint_unix_ms: None,
                tracking_started_unix_ms: None,
                bucket_start_covered: false,
                definition: USAGE_DEFINITION.to_owned(),
            },
            applications: Vec::new(),
        }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != USAGE_SCHEMA_VERSION {
            return Err("usage_schema_must_be_3");
        }
        self.query.validate()?;
        if self.applications.len() > MAX_USAGE_APPLICATIONS {
            return Err("usage_applications_exceeds_1024");
        }
        if self.coverage.definition != USAGE_DEFINITION {
            return Err("usage_definition_invalid");
        }
        if self.applications.iter().any(|duration| {
            duration.app_id.is_empty()
                || duration.app_id.len() > MAX_USAGE_APP_ID_BYTES
                || duration.app_id.contains('\0')
                || duration.bucket_key != self.query.bucket_key
                || duration.timezone_id.is_empty()
                || duration.timezone_id.len() > 128
                || duration.timezone_id.contains('\0')
        }) {
            return Err("usage_application_record_invalid");
        }
        if self.status == CapabilityAvailability::Healthy
            && (!self.coverage.niri_event_stream_connected
                || !self.coverage.logind_session_available
                || self.coverage.status != CapabilityAvailability::Healthy
                || self.captured_at_unix_ms.is_none()
                || self.coverage.last_checkpoint_unix_ms.is_none()
                || self.coverage.tracking_started_unix_ms.is_none()
                || !self.coverage.bucket_start_covered)
        {
            return Err("usage_healthy_state_missing_coverage");
        }
        Ok(())
    }
}

fn valid_daily_key(bytes: &[u8]) -> bool {
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && ascii_digits(&bytes[..4])
        && two_digits_in_range(&bytes[5..7], 1, 12)
        && two_digits_in_range(&bytes[8..10], 1, 31)
}

fn valid_weekly_key(bytes: &[u8]) -> bool {
    bytes.len() == 8
        && bytes[4] == b'-'
        && bytes[5] == b'W'
        && ascii_digits(&bytes[..4])
        && two_digits_in_range(&bytes[6..8], 1, 53)
}

fn ascii_digits(bytes: &[u8]) -> bool {
    bytes.iter().all(u8::is_ascii_digit)
}

fn two_digits_in_range(bytes: &[u8], minimum: u8, maximum: u8) -> bool {
    if bytes.len() != 2 || !ascii_digits(bytes) {
        return false;
    }
    let value = (bytes[0] - b'0') * 10 + (bytes[1] - b'0');
    (minimum..=maximum).contains(&value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn daily_query() -> UsageSummaryQuery {
        UsageSummaryQuery {
            period: UsagePeriod::Daily,
            bucket_key: "2026-08-09".to_owned(),
        }
    }

    #[test]
    fn unavailable_summary_does_not_invent_time_or_usage() {
        let summary = UsageSummary::unavailable(daily_query(), "niri_unavailable", true);

        assert_eq!(summary.schema_version, USAGE_SCHEMA_VERSION);
        assert_eq!(summary.captured_at_unix_ms, None);
        assert!(summary.applications.is_empty());
        assert_eq!(summary.validate(), Ok(()));
    }

    #[test]
    fn daily_and_weekly_keys_are_typed_and_bounded() {
        assert_eq!(daily_query().validate(), Ok(()));
        assert_eq!(
            UsageSummaryQuery {
                period: UsagePeriod::Weekly,
                bucket_key: "2026-W32".to_owned(),
            }
            .validate(),
            Ok(())
        );
        assert_eq!(
            UsageSummaryQuery {
                period: UsagePeriod::Daily,
                bucket_key: "2026-W32".to_owned(),
            }
            .validate(),
            Err("usage_bucket_key_invalid")
        );
    }
}
