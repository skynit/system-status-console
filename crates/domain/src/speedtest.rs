use serde::{Deserialize, Serialize};

pub const SPEEDTEST_SCHEMA_VERSION: u16 = 1;
pub const SPEEDTEST_LATENCY_PROBES_PER_TARGET: usize = 3;
pub const SPEEDTEST_MAX_LATENCY_TARGETS: usize = 16;
pub const SPEEDTEST_MAX_MIRRORS: usize = 8;
pub const SPEEDTEST_MAX_BANDWIDTH_MEASUREMENTS: usize = SPEEDTEST_MAX_MIRRORS + 1;
pub const SPEEDTEST_MAX_REASON_BYTES: usize = 128;

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeedTestStage {
    Latency,
    Bandwidth,
    IpPurity,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LatencyProbe {
    pub connect_ms: Option<u32>,
    pub ttfb_ms: Option<u32>,
    pub http_code: Option<u16>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LatencyTargetResult {
    pub host: String,
    pub probes: Vec<LatencyProbe>,
    pub avg_ttfb_ms: Option<u32>,
}

impl LatencyTargetResult {
    pub fn validate(&self) -> bool {
        !self.host.is_empty()
            && self.host.len() <= 253
            && !self.probes.is_empty()
            && self.probes.len() <= SPEEDTEST_LATENCY_PROBES_PER_TARGET
            && self
                .probes
                .iter()
                .all(|probe| probe.error.as_deref().is_none_or(|e| e.len() <= SPEEDTEST_MAX_REASON_BYTES))
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BandwidthKind {
    International,
    Domestic,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BandwidthMeasurement {
    pub kind: BandwidthKind,
    pub label: String,
    pub source: String,
    pub download_bits_per_second: Option<u64>,
    pub upload_bits_per_second: Option<u64>,
    pub http_code: Option<u16>,
    pub error: Option<String>,
}

impl BandwidthMeasurement {
    pub fn validate(&self) -> bool {
        !self.label.is_empty()
            && self.label.len() <= 128
            && !self.source.is_empty()
            && self.source.len() <= 512
            && self.error.as_deref().is_none_or(|e| e.len() <= SPEEDTEST_MAX_REASON_BYTES)
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IpRiskSource {
    pub source: String,
    pub risk: Option<u32>,
    pub weight: Option<f64>,
}

impl IpRiskSource {
    pub fn validate(&self) -> bool {
        !self.source.is_empty()
            && self.source.len() <= 64
            && self.risk.is_none_or(|risk| risk <= 100)
            && self.weight.is_none_or(|weight| (0.0..=1.0).contains(&weight))
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IpPurityResult {
    pub source: String,
    pub ip: Option<String>,
    pub country: Option<String>,
    pub region: Option<String>,
    pub city: Option<String>,
    pub isp: Option<String>,
    pub org: Option<String>,
    pub asn: Option<String>,
    pub asname: Option<String>,
    pub proxy: Option<bool>,
    pub hosting: Option<bool>,
    pub mobile: Option<bool>,
    /// Risk score 0-100 from the ipok.io public API (7 weighted sources).
    pub risk_score: Option<u32>,
    /// ipok.io ipType/usageType, e.g. "native" or "hosting".
    pub ip_type: Option<String>,
    /// ipok.io signals, e.g. ["hosting"].
    pub signals: Vec<String>,
    /// ipok.io riskBreakdown contributors.
    pub risk_sources: Vec<IpRiskSource>,
    pub blocklist_checked: Option<u32>,
    pub blocklist_listed: Vec<String>,
    /// Failure reason for the risk query (base ip-api facts may still be present).
    pub risk_error: Option<String>,
    pub error: Option<String>,
}

impl IpPurityResult {
    pub fn validate(&self) -> bool {
        !self.source.is_empty()
            && self.source.len() <= 128
            && self.error.as_deref().is_none_or(|e| e.len() <= SPEEDTEST_MAX_REASON_BYTES)
            && self.risk_score.is_none_or(|score| score <= 100)
            && self.risk_sources.len() <= 8
            && self.risk_sources.iter().all(IpRiskSource::validate)
            && self.signals.len() <= 8
            && self.signals.iter().all(|s| !s.is_empty() && s.len() <= 64)
            && self.blocklist_listed.len() <= 16
            && self
                .blocklist_listed
                .iter()
                .all(|s| !s.is_empty() && s.len() <= 64)
            && self
                .risk_error
                .as_deref()
                .is_none_or(|e| e.len() <= SPEEDTEST_MAX_REASON_BYTES)
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(tag = "stage", content = "payload", rename_all = "snake_case")]
pub enum SpeedTestStageData {
    Latency { targets: Vec<LatencyTargetResult> },
    Bandwidth { measurements: Vec<BandwidthMeasurement> },
    IpPurity { purity: IpPurityResult },
}

impl SpeedTestStageData {
    pub fn stage(&self) -> SpeedTestStage {
        match self {
            Self::Latency { .. } => SpeedTestStage::Latency,
            Self::Bandwidth { .. } => SpeedTestStage::Bandwidth,
            Self::IpPurity { .. } => SpeedTestStage::IpPurity,
        }
    }

    pub fn validate(&self) -> bool {
        match self {
            Self::Latency { targets } => {
                !targets.is_empty()
                    && targets.len() <= SPEEDTEST_MAX_LATENCY_TARGETS
                    && targets.iter().all(LatencyTargetResult::validate)
            }
            Self::Bandwidth { measurements } => {
                !measurements.is_empty()
                    && measurements.len() <= SPEEDTEST_MAX_BANDWIDTH_MEASUREMENTS
                    && measurements.iter().all(BandwidthMeasurement::validate)
            }
            Self::IpPurity { purity } => purity.validate(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SpeedTestBasicEnd {
    pub schema_version: u16,
    pub started_at_unix_ms: i64,
    pub ended_at_unix_ms: i64,
    pub stages: Vec<SpeedTestStageData>,
    pub cancelled: bool,
    pub error: Option<String>,
}

impl SpeedTestBasicEnd {
    pub fn validate(&self) -> bool {
        self.schema_version == SPEEDTEST_SCHEMA_VERSION
            && self.stages.len() <= 3
            && self.stages.iter().all(SpeedTestStageData::validate)
            && self
                .error
                .as_deref()
                .is_none_or(|e| e.len() <= SPEEDTEST_MAX_REASON_BYTES)
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Iperf3Direction {
    Download,
    Upload,
    Bidirectional,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "command", content = "params", rename_all = "snake_case")]
pub enum SpeedTestDeepCommand {
    Iperf3Start {
        server: String,
        port: u16,
        direction: Iperf3Direction,
        duration_secs: u16,
        parallel: u8,
    },
    Iperf3Stop,
    WifiScan,
    LinssidLaunch,
}

impl SpeedTestDeepCommand {
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Iperf3Start {
                server,
                port,
                direction: _,
                duration_secs,
                parallel,
            } => {
                if server.is_empty()
                    || server.len() > 253
                    || server.chars().any(char::is_control)
                    || server.chars().any(char::is_whitespace)
                {
                    return Err("iperf3_server_invalid");
                }
                if *port == 0 {
                    return Err("iperf3_port_invalid");
                }
                if *duration_secs == 0 || *duration_secs > 60 {
                    return Err("iperf3_duration_out_of_range");
                }
                if *parallel == 0 || *parallel > 8 {
                    return Err("iperf3_parallel_out_of_range");
                }
                Ok(())
            }
            Self::Iperf3Stop | Self::WifiScan | Self::LinssidLaunch => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Iperf3Result {
    pub server: String,
    pub port: u16,
    pub direction: Iperf3Direction,
    pub duration_secs: u16,
    pub parallel: u8,
    pub started_at_unix_ms: i64,
    pub ended_at_unix_ms: i64,
    pub download_bits_per_second: Option<u64>,
    pub upload_bits_per_second: Option<u64>,
    pub retransmits: Option<u64>,
    pub jitter_ms: Option<f64>,
    pub error: Option<String>,
}

impl Iperf3Result {
    pub fn validate(&self) -> bool {
        self.server.len() <= 253
            && self
                .error
                .as_deref()
                .is_none_or(|e| e.len() <= SPEEDTEST_MAX_REASON_BYTES)
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WifiNetwork {
    pub ssid: String,
    pub signal_percent: Option<u32>,
    pub signal_dbm: Option<i32>,
    pub signal_bars: Option<String>,
    pub channel: Option<u32>,
    pub band: Option<String>,
    pub security: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WifiScanResult {
    pub scanned_at_unix_ms: i64,
    pub source: String,
    pub networks: Vec<WifiNetwork>,
    pub error: Option<String>,
}

impl WifiScanResult {
    pub fn validate(&self) -> bool {
        self.source.len() <= 128
            && self.networks.len() <= 256
            && self
                .networks
                .iter()
                .all(|network| {
                    network.ssid.len() <= 128
                        && network.signal_percent.is_none_or(|signal| signal <= 100)
                        && network.signal_dbm.is_none_or(|signal| (-200..=100).contains(&signal))
                        && network
                            .signal_bars
                            .as_deref()
                            .is_none_or(|bars| bars.len() <= 16)
                })
            && self
                .error
                .as_deref()
                .is_none_or(|e| e.len() <= SPEEDTEST_MAX_REASON_BYTES)
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LinssidLaunchResult {
    pub launched: bool,
    pub executable: Option<String>,
    pub reason: String,
}

impl LinssidLaunchResult {
    pub fn validate(&self) -> bool {
        self.reason.len() <= SPEEDTEST_MAX_REASON_BYTES
            && self
                .executable
                .as_deref()
                .is_none_or(|e| e.len() <= 512)
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum SpeedTestDeepOutput {
    Iperf3(Iperf3Result),
    WifiScan(WifiScanResult),
    Linssid(LinssidLaunchResult),
}

impl SpeedTestDeepOutput {
    pub fn validate_for(&self, command: &SpeedTestDeepCommand) -> bool {
        match (self, command) {
            (Self::Iperf3(result), SpeedTestDeepCommand::Iperf3Start { .. }) => result.validate(),
            (Self::WifiScan(result), SpeedTestDeepCommand::WifiScan) => result.validate(),
            (Self::Linssid(result), SpeedTestDeepCommand::LinssidLaunch) => result.validate(),
            (Self::Iperf3(result), SpeedTestDeepCommand::Iperf3Stop) => {
                result.validate()
                    && result
                        .error
                        .as_deref()
                        .is_none_or(|e| e == "iperf3_not_running")
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SpeedTestCancelResult {
    pub cancelled: bool,
    pub reason: String,
}

impl SpeedTestCancelResult {
    pub fn validate(&self) -> bool {
        self.reason.len() <= SPEEDTEST_MAX_REASON_BYTES
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_command_validation_bounds() {
        let valid = SpeedTestDeepCommand::Iperf3Start {
            server: "10.0.0.8".to_owned(),
            port: 5201,
            direction: Iperf3Direction::Bidirectional,
            duration_secs: 10,
            parallel: 2,
        };
        assert!(valid.validate().is_ok());
        assert!(SpeedTestDeepCommand::Iperf3Stop.validate().is_ok());

        let empty_server = SpeedTestDeepCommand::Iperf3Start {
            server: String::new(),
            port: 5201,
            direction: Iperf3Direction::Download,
            duration_secs: 10,
            parallel: 1,
        };
        assert_eq!(empty_server.validate(), Err("iperf3_server_invalid"));

        let bad_port = SpeedTestDeepCommand::Iperf3Start {
            server: "h".to_owned(),
            port: 0,
            direction: Iperf3Direction::Upload,
            duration_secs: 10,
            parallel: 1,
        };
        assert_eq!(bad_port.validate(), Err("iperf3_port_invalid"));

        let long_duration = SpeedTestDeepCommand::Iperf3Start {
            server: "h".to_owned(),
            port: 5201,
            direction: Iperf3Direction::Upload,
            duration_secs: 61,
            parallel: 1,
        };
        assert_eq!(long_duration.validate(), Err("iperf3_duration_out_of_range"));

        let many_parallel = SpeedTestDeepCommand::Iperf3Start {
            server: "h".to_owned(),
            port: 5201,
            direction: Iperf3Direction::Upload,
            duration_secs: 10,
            parallel: 9,
        };
        assert_eq!(many_parallel.validate(), Err("iperf3_parallel_out_of_range"));
    }

    #[test]
    fn stage_data_validation_accepts_measured_shapes() {
        let latency = SpeedTestStageData::Latency {
            targets: vec![LatencyTargetResult {
                host: "github.com".to_owned(),
                probes: vec![
                    LatencyProbe {
                        connect_ms: Some(1),
                        ttfb_ms: Some(1700),
                        http_code: Some(200),
                        error: None,
                    },
                    LatencyProbe {
                        connect_ms: Some(1),
                        ttfb_ms: Some(1690),
                        http_code: Some(200),
                        error: None,
                    },
                ],
                avg_ttfb_ms: Some(1695),
            }],
        };
        assert!(latency.validate());
        assert_eq!(latency.stage(), SpeedTestStage::Latency);

        let bandwidth = SpeedTestStageData::Bandwidth {
            measurements: vec![BandwidthMeasurement {
                kind: BandwidthKind::International,
                label: "国际线路".to_owned(),
                source: "speed.cloudflare.com".to_owned(),
                download_bits_per_second: Some(32_900_000),
                upload_bits_per_second: Some(16_600_000),
                http_code: Some(200),
                error: None,
            }],
        };
        assert!(bandwidth.validate());
    }

    #[test]
    fn deep_output_matches_command_kind() {
        let start = SpeedTestDeepCommand::Iperf3Start {
            server: "h".to_owned(),
            port: 5201,
            direction: Iperf3Direction::Download,
            duration_secs: 10,
            parallel: 1,
        };
        let result = Iperf3Result {
            server: "h".to_owned(),
            port: 5201,
            direction: Iperf3Direction::Download,
            duration_secs: 10,
            parallel: 1,
            started_at_unix_ms: 1,
            ended_at_unix_ms: 11,
            download_bits_per_second: Some(1_000),
            upload_bits_per_second: None,
            retransmits: Some(0),
            jitter_ms: Some(0.5),
            error: None,
        };
        assert!(SpeedTestDeepOutput::Iperf3(result).validate_for(&start));
    }
}
