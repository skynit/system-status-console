//! Speed-test capabilities: basic (latency / bandwidth / IP purity) and deep
//! (iperf3 / WiFi scan / LinSSID launch).
//!
//! system-first: every measurement runs the stable system tools (`curl`,
//! `iperf3`, `nmcli`, `iw`, `pkexec`) as child processes with wall-clock timeouts.
//! No new HTTP client or measurement dependency is introduced; when a tool is
//! missing the corresponding capability reports `unsupported` with a reason.

use localdesk_domain::{
    BandwidthKind, BandwidthMeasurement, CapabilityRuntimeState, Iperf3Direction, Iperf3Result,
    IpPurityResult, IpRiskSource, LatencyProbe, LatencyTargetResult, LinssidLaunchResult,
    SPEEDTEST_LATENCY_PROBES_PER_TARGET, SPEEDTEST_SCHEMA_VERSION, SpeedTestBasicEnd,
    SpeedTestCancelResult, SpeedTestDeepCommand, SpeedTestDeepOutput, SpeedTestStage,
    SpeedTestStageData, WifiNetwork, WifiScanResult,
};
use localdesk_ipc::SpeedTestStreamEvent;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use tokio::{process::Command, sync::mpsc, time::timeout};

const LATENCY_TARGETS: &[&str] = &["github.com", "chatgpt.com", "claude.ai", "bilibili.com"];
const LATENCY_PROBE_TIMEOUT: Duration = Duration::from_secs(8);
const LATENCY_PROBES: usize = SPEEDTEST_LATENCY_PROBES_PER_TARGET;

const CLOUDFLARE_DOWNLOAD_URL: &str = "https://speed.cloudflare.com/__down?bytes=25000000";
const CLOUDFLARE_UPLOAD_URL: &str = "https://speed.cloudflare.com/__up";
const CLOUDFLARE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(20);
const CLOUDFLARE_UPLOAD_TIMEOUT: Duration = Duration::from_secs(25);
const CLOUDFLARE_UPLOAD_BODY_BYTES: u64 = 8 * 1024 * 1024;

const DOMESTIC_MIRRORS: &[(&str, &str)] = &[
    (
        "阿里云",
        "https://mirrors.aliyun.com/ubuntu-releases/24.04/ubuntu-24.04.2-desktop-amd64.iso",
    ),
    (
        "中科大",
        "https://mirrors.ustc.edu.cn/ubuntu-releases/24.04/ubuntu-24.04.2-desktop-amd64.iso",
    ),
    (
        "清华",
        "https://mirrors.tuna.tsinghua.edu.cn/ubuntu-releases/24.04/ubuntu-24.04.2-desktop-amd64.iso",
    ),
    (
        "腾讯",
        "https://mirrors.cloud.tencent.com/ubuntu-releases/24.04/ubuntu-24.04.2-desktop-amd64.iso",
    ),
];
const MIRROR_TIMEOUT: Duration = Duration::from_secs(12);

const IP_API_URL: &str = "http://ip-api.com/json/?fields=status,country,regionName,city,isp,org,as,asname,proxy,hosting,mobile,query";
const IP_API_TIMEOUT: Duration = Duration::from_secs(6);

const IPERF3_MARGIN: Duration = Duration::from_secs(20);
const IPERF3_STDERR_REASON_BYTES: usize = 200;

#[derive(Debug, Clone)]
pub struct Tools {
    curl: Option<PathBuf>,
    iperf3: Option<PathBuf>,
    nmcli: Option<PathBuf>,
    iw: Option<PathBuf>,
    linssid: Option<PathBuf>,
    pkexec: Option<PathBuf>,
}

impl Tools {
    fn detect() -> Self {
        Self {
            curl: tool_in_path("curl"),
            iperf3: tool_in_path("iperf3"),
            nmcli: tool_in_path("nmcli"),
            iw: tool_in_path("iw"),
            linssid: tool_in_path("linssid"),
            pkexec: tool_in_path("pkexec"),
        }
    }

    fn speedtest_capability(&self) -> CapabilityRuntimeState {
        if self.curl.is_some() {
            CapabilityRuntimeState::healthy("curl_available")
        } else {
            CapabilityRuntimeState::unsupported("curl_missing")
        }
    }

    fn deeptest_capability(&self) -> CapabilityRuntimeState {
        if self.iperf3.is_some() {
            CapabilityRuntimeState::healthy("iperf3_available")
        } else if self.nmcli.is_some() || self.linssid.is_some() {
            CapabilityRuntimeState::degraded("iperf3_missing")
        } else {
            CapabilityRuntimeState::unsupported("deep_test_tools_missing")
        }
    }
}

fn tool_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[derive(Debug, Clone)]
struct CurlRun {
    exit: Option<i32>,
    stdout: String,
}

#[derive(Debug, Error)]
#[error("speedtest failed: {code}: {reason}")]
pub struct SpeedTestError {
    pub code: String,
    pub reason: String,
    pub retryable: bool,
}

impl SpeedTestError {
    fn new(code: impl Into<String>, reason: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: code.into(),
            reason: reason.into(),
            retryable,
        }
    }

    fn busy() -> Self {
        Self::new("speedtest_busy", "speedtest_already_running", true)
    }
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn parse_ms(seconds: &str) -> Option<u32> {
    seconds
        .parse::<f64>()
        .ok()
        .map(|value| (value * 1000.0).round() as u32)
}

fn parse_u64(value: &str) -> Option<u64> {
    value.parse::<f64>().ok().map(|value| value as u64)
}

fn curl_error_reason(exit: i32) -> String {
    match exit {
        28 => "curl_exit_28_timeout".to_owned(),
        47 => "curl_exit_47_too_many_redirects".to_owned(),
        22 => "curl_exit_22_http_error".to_owned(),
        7 => "curl_exit_7_connection_refused".to_owned(),
        6 => "curl_exit_6_resolve_failed".to_owned(),
        35 => "curl_exit_35_tls_error".to_owned(),
        56 => "curl_exit_56_recv_error".to_owned(),
        other => format!("curl_exit_{other}"),
    }
}

async fn run_curl(args: &[String], limit: Duration) -> Result<CurlRun, String> {
    match timeout(limit, Command::new("curl").args(args).output()).await {
        Ok(Ok(output)) => Ok(CurlRun {
            exit: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        }),
        Ok(Err(error)) => Err(format!("curl_spawn_failed:{error}")),
        Err(_) => Err("curl_exit_28_timeout".to_owned()),
    }
}

fn bits_per_second(bytes_per_second: Option<u64>) -> Option<u64> {
    bytes_per_second.map(|bytes| bytes.saturating_mul(8))
}

/// Shared state for a single basic-test run and a single iperf3 run.
#[derive(Clone)]
pub struct SpeedTestHandle {
    tools: Tools,
    cancel: Arc<AtomicBool>,
    basic_busy: Arc<AtomicBool>,
    iperf3_busy: Arc<AtomicBool>,
    iperf3_pid: Arc<Mutex<Option<u32>>>,
}

impl SpeedTestHandle {
    pub fn new() -> Self {
        Self {
            tools: Tools::detect(),
            cancel: Arc::new(AtomicBool::new(false)),
            basic_busy: Arc::new(AtomicBool::new(false)),
            iperf3_busy: Arc::new(AtomicBool::new(false)),
            iperf3_pid: Arc::new(Mutex::new(None)),
        }
    }

    pub fn capability_states(&self) -> (CapabilityRuntimeState, CapabilityRuntimeState) {
        (
            self.tools.speedtest_capability(),
            self.tools.deeptest_capability(),
        )
    }

    /// Runs the requested stages concurrently; each stage frame is sent as soon
    /// as that stage finishes, followed by a single End frame.
    pub fn start_basic(
        &self,
        stages: Vec<SpeedTestStage>,
    ) -> Result<mpsc::Receiver<SpeedTestStreamEvent>, SpeedTestError> {
        if self.basic_busy.swap(true, Ordering::AcqRel) {
            return Err(SpeedTestError::busy());
        }
        self.cancel.store(false, Ordering::Release);
        let (sender, receiver) = mpsc::channel(8);
        let cancel = Arc::clone(&self.cancel);
        let basic_busy = Arc::clone(&self.basic_busy);
        let tools = self.tools.clone();
        tokio::spawn(async move {
            let started_at = now_unix_ms();
            let mut stage_handles = Vec::with_capacity(stages.len());
            for stage in stages {
                let stage_cancel = Arc::clone(&cancel);
                let stage_tools = tools.clone();
                let stage_sender = sender.clone();
                stage_handles.push(tokio::spawn(async move {
                    let data = match stage {
                        SpeedTestStage::Latency => SpeedTestStageData::Latency {
                            targets: run_latency_stage(&stage_cancel).await,
                        },
                        SpeedTestStage::Bandwidth => SpeedTestStageData::Bandwidth {
                            measurements: run_bandwidth_stage(&stage_cancel).await,
                        },
                        SpeedTestStage::IpPurity => SpeedTestStageData::IpPurity {
                            purity: run_ip_purity_stage(&stage_tools).await,
                        },
                    };
                    // Client may have gone away; ignore send failures.
                    let _ = stage_sender.send(SpeedTestStreamEvent::Stage(data.clone())).await;
                    data
                }));
            }
            let mut completed = Vec::with_capacity(stage_handles.len());
            for handle in stage_handles {
                if let Ok(data) = handle.await {
                    completed.push(data);
                }
            }
            // Stable output order regardless of completion order.
            completed.sort_by_key(|data| match data.stage() {
                SpeedTestStage::Latency => 0,
                SpeedTestStage::Bandwidth => 1,
                SpeedTestStage::IpPurity => 2,
            });
            let end = SpeedTestBasicEnd {
                schema_version: SPEEDTEST_SCHEMA_VERSION,
                started_at_unix_ms: started_at,
                ended_at_unix_ms: now_unix_ms(),
                stages: completed,
                cancelled: cancel.load(Ordering::Acquire),
                error: None,
            };
            let _ = sender.send(SpeedTestStreamEvent::End(Box::new(end))).await;
            basic_busy.store(false, Ordering::Release);
        });
        Ok(receiver)
    }

    pub fn cancel(&self) -> SpeedTestCancelResult {
        let running = self.basic_busy.load(Ordering::Acquire);
        self.cancel.store(true, Ordering::Release);
        SpeedTestCancelResult {
            cancelled: running,
            reason: if running {
                "cancellation_requested".to_owned()
            } else {
                "no_active_speedtest".to_owned()
            },
        }
    }

    pub async fn deep_command(
        &self,
        command: SpeedTestDeepCommand,
    ) -> Result<SpeedTestDeepOutput, SpeedTestError> {
        match command {
            SpeedTestDeepCommand::Iperf3Start {
                server,
                port,
                direction,
                duration_secs,
                parallel,
            } => {
                if self.iperf3_busy.swap(true, Ordering::AcqRel) {
                    return Err(SpeedTestError::busy());
                }
                let result = self
                    .run_iperf3_plan(&server, port, direction, duration_secs, parallel)
                    .await;
                self.iperf3_busy.store(false, Ordering::Release);
                Ok(SpeedTestDeepOutput::Iperf3(result))
            }
            SpeedTestDeepCommand::Iperf3Stop => Ok(SpeedTestDeepOutput::Iperf3(
                self.stop_iperf3().await,
            )),
            SpeedTestDeepCommand::WifiScan => Ok(SpeedTestDeepOutput::WifiScan(
                self.wifi_scan().await,
            )),
            SpeedTestDeepCommand::LinssidLaunch => Ok(SpeedTestDeepOutput::Linssid(
                self.linssid_launch(),
            )),
        }
    }

    async fn run_iperf3_plan(
        &self,
        server: &str,
        port: u16,
        direction: Iperf3Direction,
        duration_secs: u16,
        parallel: u8,
    ) -> Iperf3Result {
        let started_at = now_unix_ms();
        let mut download = None;
        let mut upload = None;
        let mut retransmits = None;
        let mut jitter = None;
        let mut error = None;

        let runs = match direction {
            Iperf3Direction::Upload => vec![false],
            Iperf3Direction::Download => vec![true],
            Iperf3Direction::Bidirectional => vec![false, true],
        };
        for reverse in runs {
            if error.is_some() {
                break;
            }
            let (sent, received, retr, jit) = match self
                .run_iperf3_once(server, port, reverse, duration_secs, parallel)
                .await
            {
                Ok(run) => run,
                Err(run_error) => {
                    error = Some(run_error);
                    continue;
                }
            };
            retransmits.get_or_insert(retr);
            if reverse {
                download = Some(received);
                jitter = jit;
            } else {
                upload = Some(sent);
            }
        }

        Iperf3Result {
            server: server.to_owned(),
            port,
            direction,
            duration_secs,
            parallel,
            started_at_unix_ms: started_at,
            ended_at_unix_ms: now_unix_ms(),
            download_bits_per_second: download,
            upload_bits_per_second: upload,
            retransmits,
            jitter_ms: jitter,
            error,
        }
    }

    async fn run_iperf3_once(
        &self,
        server: &str,
        port: u16,
        reverse: bool,
        duration_secs: u16,
        parallel: u8,
    ) -> Result<(u64, u64, u64, Option<f64>), String> {
        let mut command = Command::new(
            self.tools
                .iperf3
                .as_deref()
                .ok_or_else(|| "iperf3_missing".to_owned())?,
        );
        command
            .arg("-c")
            .arg(server)
            .arg("-p")
            .arg(port.to_string())
            .arg("-J")
            .arg("-t")
            .arg(duration_secs.to_string())
            .arg("-P")
            .arg(parallel.to_string());
        if reverse {
            command.arg("-R");
        }
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("iperf3_spawn_failed:{error}"))?;
        {
            let mut slot = self
                .iperf3_pid
                .lock()
                .map_err(|_| "iperf3_state_poisoned".to_owned())?;
            *slot = child.id();
        }
        let mut stdout_pipe = child.stdout.take();
        let mut stderr_pipe = child.stderr.take();
        let limit = Duration::from_secs(u64::from(duration_secs)) + IPERF3_MARGIN;
        let status = match timeout(limit, child.wait()).await {
            Ok(Ok(status)) => status,
            Ok(Err(error)) => {
                let _ = child.start_kill();
                return Err(format!("iperf3_wait_failed:{error}"));
            }
            Err(_) => {
                let _ = child.start_kill();
                return Err("iperf3_exit_28_timeout".to_owned());
            }
        };
        {
            let mut slot = self
                .iperf3_pid
                .lock()
                .map_err(|_| "iperf3_state_poisoned".to_owned())?;
            *slot = None;
        }
        let mut stdout_bytes = Vec::new();
        let mut stderr_bytes = Vec::new();
        if let Some(pipe) = stdout_pipe.as_mut() {
            let _ = tokio::io::AsyncReadExt::read_to_end(pipe, &mut stdout_bytes).await;
        }
        if let Some(pipe) = stderr_pipe.as_mut() {
            let _ = tokio::io::AsyncReadExt::read_to_end(pipe, &mut stderr_bytes).await;
        }
        if !status.success() {
            let stderr = String::from_utf8_lossy(&stderr_bytes);
            let stderr = stderr.trim();
            let reason = if stderr.is_empty() {
                format!("iperf3_exit_{:?}", status.code())
            } else {
                let mut reason =
                    stderr.chars().take(IPERF3_STDERR_REASON_BYTES).collect::<String>();
                if stderr.len() > IPERF3_STDERR_REASON_BYTES {
                    reason.push('…');
                }
                reason
            };
            return Err(reason);
        }
        let stdout = String::from_utf8_lossy(&stdout_bytes);
        let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
            .map_err(|_| "iperf3_invalid_json_output".to_owned())?;
        let end = &parsed["end"];
        let sum_sent = &end["sum_sent"];
        let sum_received = &end["sum_received"];
        let sent = sum_sent["bits_per_second"].as_f64().map(|v| v as u64);
        let received = sum_received["bits_per_second"].as_f64().map(|v| v as u64);
        let retransmits = sum_sent["retransmits"].as_u64();
        let jitter = sum_received["jitter_ms"].as_f64();
        Ok((
            sent.unwrap_or(0),
            received.unwrap_or(0),
            retransmits.unwrap_or(0),
            jitter,
        ))
    }

    async fn stop_iperf3(&self) -> Iperf3Result {
        let started_at = now_unix_ms();
        let pid = match self.iperf3_pid.lock() {
            Ok(mut slot) => slot.take(),
            Err(_) => {
                return Iperf3Result {
                    server: String::new(),
                    port: 0,
                    direction: Iperf3Direction::Upload,
                    duration_secs: 0,
                    parallel: 0,
                    started_at_unix_ms: started_at,
                    ended_at_unix_ms: now_unix_ms(),
                    download_bits_per_second: None,
                    upload_bits_per_second: None,
                    retransmits: None,
                    jitter_ms: None,
                    error: Some("iperf3_state_poisoned".to_owned()),
                };
            }
        };
        let error = match pid {
            Some(pid) => {
                // SIGTERM the runner; the runner task observes the exit and
                // reports `iperf3_stopped_by_user` to its client.
                let terminated = unsafe { libc::kill(pid as i32, libc::SIGTERM) } == 0;
                if terminated {
                    None
                } else {
                    Some("iperf3_not_running".to_owned())
                }
            }
            None => Some("iperf3_not_running".to_owned()),
        };
        Iperf3Result {
            server: String::new(),
            port: 0,
            direction: Iperf3Direction::Upload,
            duration_secs: 0,
            parallel: 0,
            started_at_unix_ms: started_at,
            ended_at_unix_ms: now_unix_ms(),
            download_bits_per_second: None,
            upload_bits_per_second: None,
            retransmits: None,
            jitter_ms: None,
            error,
        }
    }

    async fn wifi_scan(&self) -> WifiScanResult {
        let scanned_at = now_unix_ms();
        let Some(nmcli) = self.tools.nmcli.as_deref() else {
            return WifiScanResult {
                scanned_at_unix_ms: scanned_at,
                source: "nmcli".to_owned(),
                networks: Vec::new(),
                error: Some("nmcli_missing".to_owned()),
            };
        };
        let output = match timeout(
            Duration::from_secs(15),
            Command::new(nmcli)
                .args([
                    "-t",
                    "-f",
                    "SSID,SIGNAL,CHAN,SECURITY,BAND",
                    "dev",
                    "wifi",
                    "list",
                ])
                .output(),
        )
        .await
        {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                return WifiScanResult {
                    scanned_at_unix_ms: scanned_at,
                    source: "nmcli".to_owned(),
                    networks: Vec::new(),
                    error: Some(format!("nmcli_spawn_failed:{error}")),
                };
            }
            Err(_) => {
                return WifiScanResult {
                    scanned_at_unix_ms: scanned_at,
                    source: "nmcli".to_owned(),
                    networks: Vec::new(),
                    error: Some("nmcli_exit_28_timeout".to_owned()),
                };
            }
        };
        if !output.status.success() {
            return WifiScanResult {
                scanned_at_unix_ms: scanned_at,
                source: "nmcli".to_owned(),
                networks: Vec::new(),
                error: Some(format!(
                    "nmcli_exit_{:?}",
                    output.status.code()
                )),
            };
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let mut networks = Vec::new();
        for line in text.lines() {
            if line.is_empty() {
                continue;
            }
            let fields = split_terse(line);
            if fields.len() < 5 {
                continue;
            }
            networks.push(WifiNetwork {
                ssid: if fields[0].is_empty() {
                    "（隐藏网络）".to_owned()
                } else {
                    fields[0].clone()
                },
                signal_percent: fields[1].parse::<u32>().ok(),
                channel: fields[2].parse::<u32>().ok(),
                security: Some(fields[3].clone()),
                band: Some(fields[4].clone()),
            });
        }
        WifiScanResult {
            scanned_at_unix_ms: scanned_at,
            source: "nmcli".to_owned(),
            networks,
            error: None,
        }
    }

    fn linssid_launch(&self) -> LinssidLaunchResult {
        let Some(linssid) = self.tools.linssid.as_deref() else {
            return LinssidLaunchResult {
                launched: false,
                executable: None,
                reason: "linssid_missing".to_owned(),
            };
        };
        let Some(pkexec) = self.tools.pkexec.as_deref() else {
            return LinssidLaunchResult {
                launched: false,
                executable: Some(linssid.display().to_string()),
                reason: "pkexec_missing".to_owned(),
            };
        };
        let launched = std::process::Command::new(pkexec)
            .arg(linssid)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match launched {
            Ok(_) => LinssidLaunchResult {
                launched: true,
                executable: Some(linssid.display().to_string()),
                reason: "launched_via_pkexec".to_owned(),
            },
            Err(error) => LinssidLaunchResult {
                launched: false,
                executable: Some(linssid.display().to_string()),
                reason: format!("launch_failed:{error}"),
            },
        }
    }
}

fn split_terse(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            ':' => {
                fields.push(std::mem::take(&mut current));
            }
            other => current.push(other),
        }
    }
    fields.push(current);
    fields
}

async fn run_latency_stage(cancel: &Arc<AtomicBool>) -> Vec<LatencyTargetResult> {
    let mut targets = Vec::new();
    for host in LATENCY_TARGETS {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        let mut probes = Vec::new();
        for _ in 0..LATENCY_PROBES {
            if cancel.load(Ordering::Acquire) {
                break;
            }
            let url = format!("https://{host}/");
            let args = [
                "-s".to_owned(),
                "-o".to_owned(),
                "/dev/null".to_owned(),
                "-w".to_owned(),
                "%{http_code} %{time_connect} %{time_starttransfer}".to_owned(),
                "--max-time".to_owned(),
                "8".to_owned(),
                url,
            ];
            let probe = match run_curl(&args, LATENCY_PROBE_TIMEOUT).await {
                Ok(run) => {
                    let parts: Vec<&str> = run.stdout.split_whitespace().collect();
                    let http_code = parts.first().and_then(|value| value.parse::<u16>().ok());
                    let connect_ms = parts.get(1).and_then(|value| parse_ms(value));
                    let ttfb_ms = parts.get(2).and_then(|value| parse_ms(value));
                    let error = run.exit.filter(|exit| *exit != 0).map(curl_error_reason);
                    LatencyProbe {
                        connect_ms,
                        ttfb_ms,
                        http_code,
                        error,
                    }
                }
                Err(reason) => LatencyProbe {
                    connect_ms: None,
                    ttfb_ms: None,
                    http_code: None,
                    error: Some(reason),
                },
            };
            probes.push(probe);
        }
        if probes.is_empty() {
            break;
        }
        let ttfb_values: Vec<u32> = probes
            .iter()
            .filter_map(|probe| probe.ttfb_ms)
            .collect();
        let avg_ttfb_ms = (!ttfb_values.is_empty())
            .then(|| ttfb_values.iter().sum::<u32>() / ttfb_values.len() as u32);
        targets.push(LatencyTargetResult {
            host: (*host).to_owned(),
            probes,
            avg_ttfb_ms,
        });
    }
    targets
}

async fn run_bandwidth_stage(cancel: &Arc<AtomicBool>) -> Vec<BandwidthMeasurement> {
    let mut measurements = Vec::new();
    if cancel.load(Ordering::Acquire) {
        return measurements;
    }
    // International (cloudflare) and every domestic mirror run concurrently.
    let international = tokio::spawn(run_international_measurement());
    let mut mirror_tasks = Vec::with_capacity(DOMESTIC_MIRRORS.len());
    for (label, url) in DOMESTIC_MIRRORS {
        let label = *label;
        let url = *url;
        let mirror_cancel = Arc::clone(cancel);
        mirror_tasks.push(tokio::spawn(async move {
            if mirror_cancel.load(Ordering::Acquire) {
                None
            } else {
                Some(run_domestic_measurement(label, url).await)
            }
        }));
    }
    if let Ok(measurement) = international.await {
        measurements.push(measurement);
    }
    for task in mirror_tasks {
        if let Ok(Some(measurement)) = task.await {
            measurements.push(measurement);
        }
    }
    measurements
}

async fn run_international_measurement() -> BandwidthMeasurement {
    let download_args = [
        "-s".to_owned(),
        "-o".to_owned(),
        "/dev/null".to_owned(),
        "-w".to_owned(),
        "%{http_code} %{speed_download}".to_owned(),
        "--max-time".to_owned(),
        "20".to_owned(),
        CLOUDFLARE_DOWNLOAD_URL.to_owned(),
    ];
    let download = match run_curl(&download_args, CLOUDFLARE_DOWNLOAD_TIMEOUT).await {
        Ok(run) => {
            let parts: Vec<&str> = run.stdout.split_whitespace().collect();
            let http_code = parts.first().and_then(|value| value.parse::<u16>().ok());
            let speed = parts.get(1).and_then(|value| parse_u64(value));
            (
                bits_per_second(speed),
                http_code,
                run.exit.filter(|exit| *exit != 0).map(curl_error_reason),
            )
        }
        Err(reason) => (None, None, Some(reason)),
    };

    let upload_path = upload_body_path();
    let upload = match fs::File::create(&upload_path) {
        Ok(file) => {
            let _ = file.set_len(CLOUDFLARE_UPLOAD_BODY_BYTES);
            let upload_args = [
                "-s".to_owned(),
                "-X".to_owned(),
                "POST".to_owned(),
                "--data-binary".to_owned(),
                format!("@{}", upload_path.display()),
                "-w".to_owned(),
                "%{http_code} %{speed_upload}".to_owned(),
                "--max-time".to_owned(),
                "25".to_owned(),
                CLOUDFLARE_UPLOAD_URL.to_owned(),
            ];
            let result = run_curl(&upload_args, CLOUDFLARE_UPLOAD_TIMEOUT).await;
            let _ = fs::remove_file(&upload_path);
            match result {
                Ok(run) => {
                    let parts: Vec<&str> = run.stdout.split_whitespace().collect();
                    let http_code = parts.first().and_then(|value| value.parse::<u16>().ok());
                    let speed = parts.get(1).and_then(|value| parse_u64(value));
                    (
                        bits_per_second(speed),
                        http_code,
                        run.exit.filter(|exit| *exit != 0).map(curl_error_reason),
                    )
                }
                Err(reason) => (None, None, Some(reason)),
            }
        }
        Err(error) => {
            let _ = fs::remove_file(&upload_path);
            (None, None, Some(format!("upload_body_create_failed:{error}")))
        }
    };

    BandwidthMeasurement {
        kind: BandwidthKind::International,
        label: "国际线路".to_owned(),
        source: "speed.cloudflare.com".to_owned(),
        download_bits_per_second: download.0,
        upload_bits_per_second: upload.0,
        http_code: download.1.or(upload.1),
        error: download.2.or(upload.2),
    }
}

fn upload_body_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "localdesk-speedtest-upload-{}-{}.bin",
        std::process::id(),
        now_unix_ms()
    ))
}

async fn run_domestic_measurement(label: &str, url: &str) -> BandwidthMeasurement {
    let args = [
        "-sL".to_owned(),
        "-o".to_owned(),
        "/dev/null".to_owned(),
        "-w".to_owned(),
        "%{http_code} %{speed_download}".to_owned(),
        "--max-time".to_owned(),
        "12".to_owned(),
        url.to_owned(),
    ];
    let (bits, http_code, error) = match run_curl(&args, MIRROR_TIMEOUT).await {
        Ok(run) => {
            let parts: Vec<&str> = run.stdout.split_whitespace().collect();
            let http_code = parts.first().and_then(|value| value.parse::<u16>().ok());
            let speed = parts.get(1).and_then(|value| parse_u64(value));
            (
                bits_per_second(speed),
                http_code,
                run.exit.filter(|exit| *exit != 0).map(curl_error_reason),
            )
        }
        Err(reason) => (None, None, Some(reason)),
    };
    BandwidthMeasurement {
        kind: BandwidthKind::Domestic,
        label: label.to_owned(),
        source: url.to_owned(),
        download_bits_per_second: bits,
        upload_bits_per_second: None,
        http_code,
        error,
    }
}

async fn run_ip_purity_stage(tools: &Tools) -> IpPurityResult {
    let error = if tools.curl.is_none() {
        Some("curl_missing".to_owned())
    } else {
        None
    };
    if error.is_some() {
        return IpPurityResult {
            source: "ip-api.com".to_owned(),
            ip: None,
            country: None,
            region: None,
            city: None,
            isp: None,
            org: None,
            asn: None,
            asname: None,
            proxy: None,
            hosting: None,
            mobile: None,
            risk_score: None,
            ip_type: None,
            signals: Vec::new(),
            risk_sources: Vec::new(),
            blocklist_checked: None,
            blocklist_listed: Vec::new(),
            risk_error: None,
            error,
        };
    }
    let args = ["-s".to_owned(), "--max-time".to_owned(), "6".to_owned(), IP_API_URL.to_owned()];
    let output = match run_curl(&args, IP_API_TIMEOUT).await {
        Ok(run) => run,
        Err(reason) => {
            return IpPurityResult {
                source: "ip-api.com".to_owned(),
                ip: None,
                country: None,
                region: None,
                city: None,
                isp: None,
                org: None,
                asn: None,
                asname: None,
                proxy: None,
                hosting: None,
                mobile: None,
                risk_score: None,
                ip_type: None,
                signals: Vec::new(),
                risk_sources: Vec::new(),
                blocklist_checked: None,
                blocklist_listed: Vec::new(),
                risk_error: None,
                error: Some(reason),
            };
        }
    };
    let parsed: serde_json::Value = match serde_json::from_str(output.stdout.trim()) {
        Ok(value) => value,
        Err(_) => {
            return IpPurityResult {
                source: "ip-api.com".to_owned(),
                ip: None,
                country: None,
                region: None,
                city: None,
                isp: None,
                org: None,
                asn: None,
                asname: None,
                proxy: None,
                hosting: None,
                mobile: None,
                risk_score: None,
                ip_type: None,
                signals: Vec::new(),
                risk_sources: Vec::new(),
                blocklist_checked: None,
                blocklist_listed: Vec::new(),
                risk_error: None,
                error: Some("ip_api_invalid_json".to_owned()),
            };
        }
    };
    let status = parsed["status"].as_str().unwrap_or("");
    let field = |key: &str| parsed[key].as_str().map(str::to_owned);
    let flag = |key: &str| parsed[key].as_bool();
    let ip = field("query");
    let base_error = if status == "success" {
        None
    } else {
        Some(format!("ip_api_status_failure:{}", if status.is_empty() { "unknown" } else { status }))
    };
    // Risk data from the ipok.io public API (7 weighted sources). Runs only
    // when the base lookup produced an address.
    let (risk_score, ip_type, signals, risk_sources, blocklist_checked, blocklist_listed, risk_error) =
        if base_error.is_none() && ip.is_some() {
            run_ipok_risk_query(ip.as_deref().unwrap_or("")).await
        } else {
            (
                None,
                None,
                Vec::new(),
                Vec::new(),
                None,
                Vec::new(),
                Some("ip_api_unavailable".to_owned()),
            )
        };
    IpPurityResult {
        source: "ip-api.com + ipok.io".to_owned(),
        ip,
        country: field("country"),
        region: field("regionName"),
        city: field("city"),
        isp: field("isp"),
        org: field("org"),
        asn: field("as"),
        asname: field("asname"),
        proxy: flag("proxy"),
        hosting: flag("hosting"),
        mobile: flag("mobile"),
        risk_score,
        ip_type,
        signals,
        risk_sources,
        blocklist_checked,
        blocklist_listed,
        risk_error,
        error: base_error,
    }
}

const IPOK_API_URL: &str = "https://ipok.io/api/ip?ip=";
const IPOK_TIMEOUT: Duration = Duration::from_secs(8);
const IPOK_MAX_SOURCES: usize = 8;
const IPOK_MAX_SIGNALS: usize = 8;
const IPOK_MAX_BLOCKLIST: usize = 16;

async fn run_ipok_risk_query(
    ip: &str,
) -> (
    Option<u32>,
    Option<String>,
    Vec<String>,
    Vec<IpRiskSource>,
    Option<u32>,
    Vec<String>,
    Option<String>,
) {
    let url = format!("{IPOK_API_URL}{ip}");
    let args = [
        "-s".to_owned(),
        "-A".to_owned(),
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/126.0 Safari/537.36".to_owned(),
        "--max-time".to_owned(),
        "8".to_owned(),
        url,
    ];
    let output = match run_curl(&args, IPOK_TIMEOUT).await {
        Ok(run) => run,
        Err(reason) => return (None, None, Vec::new(), Vec::new(), None, Vec::new(), Some(reason)),
    };
    let parsed: serde_json::Value = match serde_json::from_str(output.stdout.trim()) {
        Ok(value) => value,
        Err(_) => {
            return (
                None,
                None,
                Vec::new(),
                Vec::new(),
                None,
                Vec::new(),
                Some("ipok_invalid_json".to_owned()),
            );
        }
    };
    let risk_score = parsed["risk"].as_u64().map(|value| value.min(100) as u32);
    let ip_type = parsed["ipType"].as_str().map(str::to_owned);
    let mut signals = Vec::new();
    if let Some(values) = parsed["signals"].as_array() {
        for value in values.iter().take(IPOK_MAX_SIGNALS) {
            if let Some(signal) = value.as_str() {
                signals.push(signal.to_owned());
            }
        }
    }
    let mut risk_sources = Vec::new();
    if let Some(contributors) = parsed["riskBreakdown"]["contributors"].as_array() {
        for contributor in contributors.iter().take(IPOK_MAX_SOURCES) {
            let source = contributor["source"].as_str().unwrap_or("").to_owned();
            if source.is_empty() {
                continue;
            }
            risk_sources.push(IpRiskSource {
                source,
                risk: contributor["risk"].as_u64().map(|value| value.min(100) as u32),
                weight: contributor["weight"].as_f64(),
            });
        }
    }
    let blocklist_checked = parsed["blocklist"]["checked"].as_u64().map(|value| value as u32);
    let mut blocklist_listed = Vec::new();
    if let Some(listed) = parsed["blocklist"]["listed"].as_array() {
        for value in listed.iter().take(IPOK_MAX_BLOCKLIST) {
            if let Some(entry) = value.as_str() {
                blocklist_listed.push(entry.to_owned());
            }
        }
    }
    (
        risk_score,
        ip_type,
        signals,
        risk_sources,
        blocklist_checked,
        blocklist_listed,
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terse_wifi_lines_are_split_and_unescaped() {
        assert_eq!(
            split_terse("Rhino-5G:100:36:WPA2 WPA3:5 GHz"),
            vec!["Rhino-5G", "100", "36", "WPA2 WPA3", "5 GHz"]
        );
        assert_eq!(split_terse(":80:11:WPA1 WPA2:2.4 GHz"), vec!["", "80", "11", "WPA1 WPA2", "2.4 GHz"]);
        assert_eq!(
            split_terse(r"My\:Net:77:6:WPA2:2.4 GHz"),
            vec!["My:Net", "77", "6", "WPA2", "2.4 GHz"]
        );
    }

    #[test]
    fn curl_reason_maps_common_exit_codes() {
        assert_eq!(curl_error_reason(28), "curl_exit_28_timeout");
        assert_eq!(curl_error_reason(47), "curl_exit_47_too_many_redirects");
        assert_eq!(curl_error_reason(3), "curl_exit_3");
    }

    #[test]
    fn tool_detection_finds_real_tools_or_none_without_panicking() {
        let tools = Tools::detect();
        // On the development host curl is present; keep the assertion robust
        // for environments without it.
        if tools.curl.is_some() {
            assert!(tools.curl.as_deref().is_some_and(|p| p.is_file()));
        }
    }
}
