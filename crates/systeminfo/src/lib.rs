//! Normalized system facts collected via the system `fastfetch` tool.
//!
//! system-first: fastfetch is the established, actively maintained system
//! information tool on Linux. The daemon runs it with a fixed curated
//! structure and a wall-clock timeout, then normalizes its JSON output into
//! stable key/value facts the desktop UI renders. No telemetry values are
//! invented here; everything comes from the real `fastfetch` run.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use thiserror::Error;
use tokio::process::Command;

pub const SYSTEM_INFO_SCHEMA_VERSION: u16 = 1;
pub const FASTFETCH_COLLECT_TIMEOUT: Duration = Duration::from_secs(6);
pub const MAX_FASTFETCH_OUTPUT_BYTES: usize = 256 * 1024;

/// Curated `--structure` list; every id must have a builder below.
const FASTFETCH_STRUCTURE: &str =
    "OS:Host:Kernel:Uptime:Packages:Shell:WM:Display:CPU:GPU:Memory:Swap:Disk:LocalIp:Battery:Locale";

/// Canonical display order for normalized sections.
const CANONICAL_SECTIONS: &[&str] = &[
    "OS", "Host", "Kernel", "Uptime", "Packages", "Shell", "WM", "Display", "CPU", "GPU",
    "Memory", "Swap", "Disk", "LocalIp", "Battery", "Locale",
];

/// Candidate locations for the fastfetch binary (PATH lookup is last).
const FASTFETCH_CANDIDATES: &[&str] = &[
    "/usr/sbin/fastfetch",
    "/usr/bin/fastfetch",
    "/bin/fastfetch",
    "/usr/local/bin/fastfetch",
];

// ---------------------------------------------------------------------------
// Wire-shaped types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SystemInfoSection {
    /// fastfetch section id, e.g. `"CPU"`.
    pub id: String,
    /// One group per logical device; scalar sections have a single group.
    pub groups: Vec<SystemInfoGroup>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SystemInfoGroup {
    /// Device identifier (disk device, GPU name, ...) when the section has
    /// more than one device; `None` for scalar or single-device sections.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub entries: Vec<SystemInfoEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SystemInfoEntry {
    /// Stable backend-defined key, e.g. `"uptime"`.
    pub key: String,
    /// Human-formatted fact value, e.g. `"1 年 11 个月"`.
    pub value: String,
}

impl SystemInfoEntry {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemInfoStatus {
    Healthy,
    Degraded,
    Unsupported,
}

#[derive(Debug, Clone)]
pub struct SystemInfoOutcome {
    pub status: SystemInfoStatus,
    pub reason: String,
    pub retryable: bool,
    pub captured_at_unix_ms: Option<i64>,
    pub tool_version: Option<String>,
    pub sections: Vec<SystemInfoSection>,
}

impl SystemInfoOutcome {
    fn healthy(
        captured_at_unix_ms: i64,
        tool_version: Option<String>,
        sections: Vec<SystemInfoSection>,
    ) -> Self {
        Self {
            status: SystemInfoStatus::Healthy,
            reason: "fastfetch_ok".to_owned(),
            retryable: false,
            captured_at_unix_ms: Some(captured_at_unix_ms),
            tool_version,
            sections,
        }
    }

    fn degraded(reason: impl Into<String>) -> Self {
        Self {
            status: SystemInfoStatus::Degraded,
            reason: reason.into(),
            retryable: true,
            captured_at_unix_ms: now_unix_ms(),
            tool_version: None,
            sections: Vec::new(),
        }
    }

    fn unsupported(reason: impl Into<String>) -> Self {
        Self {
            status: SystemInfoStatus::Unsupported,
            reason: reason.into(),
            retryable: false,
            captured_at_unix_ms: now_unix_ms(),
            tool_version: None,
            sections: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Collector
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SystemInfoCollector {
    binary: Option<PathBuf>,
    timeout: Duration,
}

impl Default for SystemInfoCollector {
    fn default() -> Self {
        Self {
            binary: None,
            timeout: FASTFETCH_COLLECT_TIMEOUT,
        }
    }
}

impl SystemInfoCollector {
    /// Override the fastfetch binary path (used by tests with a fake binary).
    pub fn with_binary(mut self, binary: impl Into<PathBuf>) -> Self {
        self.binary = Some(binary.into());
        self
    }

    /// Override the per-run wall-clock timeout (used by timeout tests).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub async fn collect(&self) -> SystemInfoOutcome {
        let captured_at = now_unix_ms();
        let Some(binary) = self.resolve_binary() else {
            return SystemInfoOutcome::unsupported("fastfetch_not_found");
        };

        let tool_version = match run(&binary, &["--version"], self.timeout).await {
            Ok(output) => parse_tool_version(&output),
            Err(_) => None,
        };

        let output = match run(
            &binary,
            &["--format", "json", "--structure", FASTFETCH_STRUCTURE],
            self.timeout,
        )
        .await
        {
            Ok(output) => output,
            Err(error) => return SystemInfoOutcome::degraded(run_reason(&error)),
        };

        let sections = match normalize(&output) {
            Ok(sections) => sections,
            Err(error) => return SystemInfoOutcome::degraded(normalize_reason(&error)),
        };

        SystemInfoOutcome::healthy(captured_at.unwrap_or(0), tool_version, sections)
    }

    fn resolve_binary(&self) -> Option<PathBuf> {
        if let Some(binary) = &self.binary {
            return binary.is_file().then(|| binary.clone());
        }
        FASTFETCH_CANDIDATES
            .iter()
            .find(|candidate| Path::new(candidate).is_file())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("PATH").and_then(|paths| {
                    std::env::split_paths(&paths).find_map(|dir| {
                        let candidate = dir.join("fastfetch");
                        candidate.is_file().then_some(candidate)
                    })
                })
            })
    }
}

fn parse_tool_version(output: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(output);
    let first = text.lines().next()?.trim();
    if first.is_empty() || first.starts_with('[') {
        return None;
    }
    Some(first.to_owned())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
enum RunError {
    #[error("spawn failed")]
    Spawn,
    #[error("wait failed")]
    Wait,
    #[error("timed out")]
    Timeout,
    #[error("exit status {0}")]
    Exit(i32),
    #[error("output too large")]
    TooLarge,
}

fn run_reason(error: &RunError) -> &'static str {
    match error {
        RunError::Spawn => "fastfetch_spawn_failed",
        RunError::Wait => "fastfetch_failed",
        RunError::Timeout => "fastfetch_timeout",
        RunError::Exit(_) => "fastfetch_exit_nonzero",
        RunError::TooLarge => "fastfetch_output_too_large",
    }
}

async fn run(binary: &Path, args: &[&str], timeout: Duration) -> Result<Vec<u8>, RunError> {
    let child = Command::new(binary)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| RunError::Spawn)?;
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(_)) => return Err(RunError::Wait),
        Err(_) => return Err(RunError::Timeout),
    };
    if !output.status.success() {
        return Err(RunError::Exit(output.status.code().unwrap_or(-1)));
    }
    if output.stdout.len() > MAX_FASTFETCH_OUTPUT_BYTES {
        return Err(RunError::TooLarge);
    }
    Ok(output.stdout)
}

fn now_unix_ms() -> Option<i64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis() as i64)
}

// ---------------------------------------------------------------------------
// Normalizer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
enum NormalizeError {
    #[error("not an array")]
    NotArray,
    #[error("section has no object result")]
    NoObjectResult,
}

fn normalize_reason(error: &NormalizeError) -> &'static str {
    match error {
        NormalizeError::NotArray | NormalizeError::NoObjectResult => "fastfetch_output_invalid",
    }
}

fn normalize(output: &[u8]) -> Result<Vec<SystemInfoSection>, NormalizeError> {
    let value: serde_json::Value =
        serde_json::from_slice(output).map_err(|_| NormalizeError::NotArray)?;
    let array = value.as_array().ok_or(NormalizeError::NotArray)?;

    let mut by_type: std::collections::HashMap<&str, &serde_json::Value> =
        std::collections::HashMap::new();
    for section in array {
        let Some(kind) = section.get("type").and_then(|value| value.as_str()) else {
            continue;
        };
        if section.get("error").is_some_and(|value| !value.is_null()) {
            continue;
        }
        by_type.insert(kind, section);
    }

    let mut sections = Vec::new();
    for id in CANONICAL_SECTIONS {
        let Some(raw) = by_type.get(*id) else { continue };
        let Some(builder) = section_builder(id) else { continue };
        if let Ok(groups) = builder(raw) {
            if !groups.is_empty() {
                sections.push(SystemInfoSection {
                    id: (*id).to_owned(),
                    groups,
                });
            }
        }
    }
    Ok(sections)
}

type SectionBuilder = fn(&serde_json::Value) -> Result<Vec<SystemInfoGroup>, NormalizeError>;

fn section_builder(id: &str) -> Option<SectionBuilder> {
    Some(match id {
        "OS" => build_os,
        "Host" => build_host,
        "Kernel" => build_kernel,
        "Uptime" => build_uptime,
        "Packages" => build_packages,
        "Shell" => build_shell,
        "WM" => build_wm,
        "Display" => build_display,
        "CPU" => build_cpu,
        "GPU" => build_gpu,
        "Memory" => build_memory,
        "Swap" => build_swap,
        "Disk" => build_disk,
        "LocalIp" => build_local_ip,
        "Battery" => build_battery,
        "Locale" => build_locale,
        _ => return None,
    })
}

// --- JSON helpers ----------------------------------------------------------

fn result_obj(section: &serde_json::Value) -> Option<&serde_json::Value> {
    section.get("result").filter(|value| !value.is_null())
}

fn string_of(value: &serde_json::Value, key: &str) -> Option<String> {
    let text = value.get(key)?.as_str()?.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

fn object_of<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    value.get(key).filter(|value| value.is_object())
}

fn array_of<'a>(value: &'a serde_json::Value) -> Option<Vec<&'a serde_json::Value>> {
    value.as_array().map(|items| items.iter().collect())
}

fn u64_of(value: &serde_json::Value, key: &str) -> Option<u64> {
    value.get(key)?.as_u64()
}

fn f64_of(value: &serde_json::Value, key: &str) -> Option<f64> {
    value.get(key)?.as_f64()
}

fn string_array_of(value: &serde_json::Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str())
                .filter(|item| !item.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Join non-empty parts with the given separator.
fn join_parts(separator: &str, parts: impl IntoIterator<Item = String>) -> Option<String> {
    let parts: Vec<String> = parts.into_iter().filter(|part| !part.is_empty()).collect();
    (!parts.is_empty()).then(|| parts.join(separator))
}

// --- value formatters ------------------------------------------------------

fn format_bytes(value: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    const TIB: f64 = GIB * 1024.0;
    let bytes = value as f64;
    if bytes >= TIB {
        format!("{:.1} TiB", bytes / TIB)
    } else if bytes >= GIB {
        format!("{:.1} GiB", bytes / GIB)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes / MIB)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes / KIB)
    } else {
        format!("{value} B")
    }
}

fn format_percent(used: u64, total: u64) -> String {
    if total == 0 {
        return "0%".to_owned();
    }
    format!("{}%", (used as f64 * 100.0 / total as f64).round() as u64)
}

fn format_mhz(value: u64) -> String {
    if value >= 1000 {
        format!("{:.1} GHz", value as f64 / 1000.0)
    } else {
        format!("{value} MHz")
    }
}

fn format_uptime(seconds: u64) -> String {
    const YEAR: u64 = 365 * 24 * 60 * 60;
    const MONTH: u64 = 30 * 24 * 60 * 60;
    const DAY: u64 = 24 * 60 * 60;
    const HOUR: u64 = 60 * 60;
    const MINUTE: u64 = 60;

    let mut remaining = seconds;
    let years = remaining / YEAR;
    remaining %= YEAR;
    let months = remaining / MONTH;
    remaining %= MONTH;
    let days = remaining / DAY;
    remaining %= DAY;
    let hours = remaining / HOUR;
    remaining %= HOUR;
    let minutes = remaining / MINUTE;
    let seconds = remaining % MINUTE;

    let mut units = Vec::new();
    if years > 0 {
        units.push(format!("{years} 年"));
    }
    if months > 0 {
        units.push(format!("{months} 个月"));
    }
    if days > 0 {
        units.push(format!("{days} 天"));
    }
    if hours > 0 {
        units.push(format!("{hours} 小时"));
    }
    if minutes > 0 {
        units.push(format!("{minutes} 分钟"));
    }
    if seconds > 0 {
        units.push(format!("{seconds} 秒"));
    }
    match units.len() {
        0 => "刚刚".to_owned(),
        1 => units[0].clone(),
        _ => format!("{} {}", units[0], units[1]),
    }
}

fn format_iso_timestamp(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return value.to_owned();
    }
    // Offset (e.g. "+0800") always trails the fraction, if present.
    let (body, offset) = match value.rfind('+').or_else(|| value.rfind('-')) {
        Some(index)
            if index > 10
                && value[index + 1..]
                    .chars()
                    .all(|character| character.is_ascii_digit() || character == ':') =>
        {
            (&value[..index], Some(&value[index..]))
        }
        _ => (value, None),
    };
    let body = match body.find('.') {
        Some(index) => &body[..index],
        None => body,
    };
    let body = body.replace('T', " ");
    match offset {
        Some(offset) => format!("{body} ({offset})"),
        None => body,
    }
}

fn format_hz(value: f64) -> String {
    format!("{} Hz", value.round() as u64)
}

// --- section builders ------------------------------------------------------

fn build_os(section: &serde_json::Value) -> Result<Vec<SystemInfoGroup>, NormalizeError> {
    let Some(result) = result_obj(section) else {
        return Err(NormalizeError::NoObjectResult);
    };
    let name = string_of(result, "name").or_else(|| string_of(result, "prettyName"));
    let version = join_parts(
        " · ",
        [string_of(result, "version"), string_of(result, "id")]
            .into_iter()
            .flatten(),
    );
    let mut entries = Vec::new();
    if let Some(name) = name {
        entries.push(SystemInfoEntry::new("os_name", name));
    }
    if let Some(version) = version {
        entries.push(SystemInfoEntry::new("os_version", version));
    }
    Ok(single_group(entries))
}

fn build_host(section: &serde_json::Value) -> Result<Vec<SystemInfoGroup>, NormalizeError> {
    let Some(result) = result_obj(section) else {
        return Err(NormalizeError::NoObjectResult);
    };
    let vendor = string_of(result, "vendor");
    let family = string_of(result, "family");
    let name = string_of(result, "name");
    let model = match (family, name) {
        (Some(family), Some(name)) if name != family => Some(format!("{family} ({name})")),
        (Some(family), _) => Some(family),
        (None, Some(name)) => Some(name),
        _ => None,
    };
    let mut entries = Vec::new();
    if let Some(vendor) = vendor {
        entries.push(SystemInfoEntry::new("vendor", vendor));
    }
    if let Some(model) = model {
        entries.push(SystemInfoEntry::new("model", model));
    }
    Ok(single_group(entries))
}

fn build_kernel(section: &serde_json::Value) -> Result<Vec<SystemInfoGroup>, NormalizeError> {
    let Some(result) = result_obj(section) else {
        return Err(NormalizeError::NoObjectResult);
    };
    let kernel = join_parts(
        " · ",
        [string_of(result, "release"), string_of(result, "architecture")]
            .into_iter()
            .flatten(),
    );
    let build = string_of(result, "version");
    let mut entries = Vec::new();
    if let Some(kernel) = kernel {
        entries.push(SystemInfoEntry::new("kernel", kernel));
    }
    if let Some(build) = build {
        entries.push(SystemInfoEntry::new("build", build));
    }
    Ok(single_group(entries))
}

fn build_uptime(section: &serde_json::Value) -> Result<Vec<SystemInfoGroup>, NormalizeError> {
    let Some(result) = result_obj(section) else {
        return Err(NormalizeError::NoObjectResult);
    };
    let uptime = u64_of(result, "uptime").map(format_uptime);
    let boot_time = string_of(result, "bootTime").map(|value| format_iso_timestamp(&value));
    let mut entries = Vec::new();
    if let Some(uptime) = uptime {
        entries.push(SystemInfoEntry::new("uptime", uptime));
    }
    if let Some(boot_time) = boot_time {
        entries.push(SystemInfoEntry::new("boot_time", boot_time));
    }
    Ok(single_group(entries))
}

fn build_packages(section: &serde_json::Value) -> Result<Vec<SystemInfoGroup>, NormalizeError> {
    let Some(result) = result_obj(section) else {
        return Err(NormalizeError::NoObjectResult);
    };
    let Some(total) = u64_of(result, "all") else {
        return Ok(Vec::new());
    };
    let mut parts = Vec::new();
    if let Some(pacman) = u64_of(result, "pacman") {
        parts.push(format!("{pacman} pacman"));
    }
    if let Some(flatpak) = u64_of(result, "flatpakSystem") {
        parts.push(format!("{flatpak} flatpak"));
    }
    parts.push(format!("合计 {total}"));
    Ok(single_group(vec![SystemInfoEntry::new(
        "packages",
        parts.join(" · "),
    )]))
}

fn build_shell(section: &serde_json::Value) -> Result<Vec<SystemInfoGroup>, NormalizeError> {
    let Some(result) = result_obj(section) else {
        return Err(NormalizeError::NoObjectResult);
    };
    let name = string_of(result, "prettyName").or_else(|| string_of(result, "exeName"));
    let version = string_of(result, "version");
    let shell = join_parts(" · ", [name, version].into_iter().flatten());
    Ok(single_group(
        shell
            .into_iter()
            .map(|value| SystemInfoEntry::new("shell", value))
            .collect(),
    ))
}

fn build_wm(section: &serde_json::Value) -> Result<Vec<SystemInfoGroup>, NormalizeError> {
    let Some(result) = result_obj(section) else {
        return Err(NormalizeError::NoObjectResult);
    };
    let name = string_of(result, "prettyName").or_else(|| string_of(result, "processName"));
    let wm = join_parts(
        " · ",
        [
            name,
            string_of(result, "version"),
            string_of(result, "protocolName"),
        ]
        .into_iter()
        .flatten(),
    );
    Ok(single_group(
        wm.into_iter()
            .map(|value| SystemInfoEntry::new("wm", value))
            .collect(),
    ))
}

fn build_display(section: &serde_json::Value) -> Result<Vec<SystemInfoGroup>, NormalizeError> {
    let Some(result) = result_obj(section) else {
        return Err(NormalizeError::NoObjectResult);
    };
    let Some(displays) = array_of(result) else {
        return Ok(Vec::new());
    };
    let mut display_rows: Vec<(Option<String>, String)> = Vec::new();
    for display in displays {
        let Some(spec) = display_spec(display) else { continue };
        display_rows.push((string_of(display, "name"), spec));
    }
    let groups = if display_rows.len() == 1 {
        let (name, spec) = display_rows.remove(0);
        let value = match name {
            Some(name) => format!("{name} · {spec}"),
            None => spec,
        };
        vec![SystemInfoGroup {
            title: None,
            entries: vec![SystemInfoEntry::new("display", value)],
        }]
    } else {
        display_rows
            .into_iter()
            .map(|(name, spec)| SystemInfoGroup {
                title: name,
                entries: vec![SystemInfoEntry::new("display", spec)],
            })
            .collect()
    };
    Ok(groups)
}

fn display_spec(display: &serde_json::Value) -> Option<String> {
    let output = object_of(display, "output")?;
    let width = u64_of(output, "width")?;
    let height = u64_of(output, "height")?;
    let mut parts = vec![match f64_of(output, "refreshRate") {
        Some(hz) => format!("{width}×{height} @ {}", format_hz(hz)),
        None => format!("{width}×{height}"),
    }];
    if let Some(dpi) = u64_of(output, "dpi") {
        parts.push(format!("dpi {dpi}"));
    }
    if let Some(kind) = string_of(display, "type") {
        parts.push(kind);
    }
    if string_of(display, "hdrStatus").as_deref() == Some("Supported") {
        parts.push("HDR".to_owned());
    }
    join_parts(" · ", parts)
}

fn build_cpu(section: &serde_json::Value) -> Result<Vec<SystemInfoGroup>, NormalizeError> {
    let Some(result) = result_obj(section) else {
        return Err(NormalizeError::NoObjectResult);
    };
    let mut entries = Vec::new();
    if let Some(cpu) = string_of(result, "cpu") {
        entries.push(SystemInfoEntry::new("cpu_name", cpu));
    }
    if let Some(cores) = object_of(result, "cores") {
        let parts = [
            u64_of(cores, "physical").map(|count| format!("{count} 物理")),
            u64_of(cores, "logical").map(|count| format!("{count} 逻辑")),
        ];
        if let Some(cores) = join_parts(" / ", parts.into_iter().flatten()) {
            entries.push(SystemInfoEntry::new("cores", cores));
        }
    }
    let frequency = object_of(result, "frequency")
        .and_then(|frequency| u64_of(frequency, "base").map(format_mhz));
    let march = string_of(result, "march");
    if let Some(frequency) = join_parts(" · ", [frequency, march].into_iter().flatten()) {
        entries.push(SystemInfoEntry::new("frequency", frequency));
    }
    if let Some(codename) = join_parts(
        " · ",
        [string_of(result, "codeName"), string_of(result, "technology")]
            .into_iter()
            .flatten(),
    ) {
        entries.push(SystemInfoEntry::new("codename", codename));
    }
    Ok(single_group(entries))
}

fn build_gpu(section: &serde_json::Value) -> Result<Vec<SystemInfoGroup>, NormalizeError> {
    let Some(result) = result_obj(section) else {
        return Err(NormalizeError::NoObjectResult);
    };
    let Some(devices) = array_of(result) else {
        return Ok(Vec::new());
    };
    let mut rows: Vec<(Option<String>, SystemInfoEntry, SystemInfoEntry)> = Vec::new();
    for device in devices {
        let name = string_of(device, "name");
        let driver = join_parts(
            " · ",
            [
                string_of(device, "driver"),
                string_of(device, "type"),
                string_of(device, "platformApi"),
            ]
            .into_iter()
            .flatten(),
        );
        if name.is_none() && driver.is_none() {
            continue;
        }
        rows.push((
            name.clone(),
            SystemInfoEntry::new("gpu", name.unwrap_or_else(|| "unknown".to_owned())),
            SystemInfoEntry::new("driver", driver.unwrap_or_else(|| "unknown".to_owned())),
        ));
    }
    let groups = if rows.len() == 1 {
        let (_, gpu, driver) = rows.remove(0);
        vec![SystemInfoGroup {
            title: None,
            entries: vec![gpu, driver],
        }]
    } else {
        rows.into_iter()
            .map(|(title, gpu, driver)| SystemInfoGroup {
                title,
                entries: vec![gpu, driver],
            })
            .collect()
    };
    Ok(groups)
}

fn build_memory(section: &serde_json::Value) -> Result<Vec<SystemInfoGroup>, NormalizeError> {
    let Some(result) = result_obj(section) else {
        return Err(NormalizeError::NoObjectResult);
    };
    let (Some(used), Some(total)) = (u64_of(result, "used"), u64_of(result, "total")) else {
        return Ok(Vec::new());
    };
    Ok(single_group(vec![SystemInfoEntry::new(
        "memory",
        format!(
            "{} / {} ({})",
            format_bytes(used),
            format_bytes(total),
            format_percent(used, total)
        ),
    )]))
}

fn build_swap(section: &serde_json::Value) -> Result<Vec<SystemInfoGroup>, NormalizeError> {
    let Some(result) = result_obj(section) else {
        return Err(NormalizeError::NoObjectResult);
    };
    let Some(devices) = array_of(result) else {
        return Ok(Vec::new());
    };
    let mut rows: Vec<(Option<String>, String)> = Vec::new();
    for device in devices {
        let (Some(used), Some(total)) = (u64_of(device, "used"), u64_of(device, "total")) else {
            continue;
        };
        rows.push((
            string_of(device, "name"),
            format!(
                "{} / {} ({})",
                format_bytes(used),
                format_bytes(total),
                format_percent(used, total)
            ),
        ));
    }
    let groups = if rows.len() == 1 {
        let (name, usage) = rows.remove(0);
        let value = match name {
            Some(name) => format!("{name} · {usage}"),
            None => usage,
        };
        vec![SystemInfoGroup {
            title: None,
            entries: vec![SystemInfoEntry::new("swap", value)],
        }]
    } else {
        rows.into_iter()
            .map(|(title, usage)| SystemInfoGroup {
                title,
                entries: vec![SystemInfoEntry::new("swap", usage)],
            })
            .collect()
    };
    Ok(groups)
}

fn build_disk(section: &serde_json::Value) -> Result<Vec<SystemInfoGroup>, NormalizeError> {
    let Some(result) = result_obj(section) else {
        return Err(NormalizeError::NoObjectResult);
    };
    let Some(devices) = array_of(result) else {
        return Ok(Vec::new());
    };
    // Btrfs subvolumes report identical byte totals; group by
    // (mount_from, filesystem, total) and merge their mountpoints.
    let mut merged: Vec<(String, Vec<String>, String, Option<String>)> = Vec::new();
    for device in devices {
        let (Some(used), Some(total)) = (
            object_of(device, "bytes").and_then(|bytes| u64_of(bytes, "used")),
            object_of(device, "bytes").and_then(|bytes| u64_of(bytes, "total")),
        ) else {
            continue;
        };
        let Some(mountpoint) = string_of(device, "mountpoint") else {
            continue;
        };
        let mount_from = string_of(device, "mountFrom").unwrap_or_else(|| "unknown".to_owned());
        let filesystem = string_of(device, "filesystem").unwrap_or_else(|| "unknown".to_owned());
        let created = string_of(device, "createTime");
        let key = format!("{mount_from}·{filesystem}·{total}");
        match merged.iter_mut().find(|(entry, _, _, _)| *entry == key) {
            Some((_, mountpoints, _, _)) => {
                if !mountpoints.iter().any(|item| item == &mountpoint) {
                    mountpoints.push(mountpoint);
                }
            }
            None => {
                let available = object_of(device, "bytes")
                    .and_then(|bytes| u64_of(bytes, "available"))
                    .map(format_bytes)
                    .unwrap_or_else(|| "unknown".to_owned());
                let usage = format!(
                    "{} / {} ({}) · 可用 {}",
                    format_bytes(used),
                    format_bytes(total),
                    format_percent(used, total),
                    available,
                );
                merged.push((key, vec![mountpoint], usage, created));
            }
        }
    }
    let mut groups: Vec<SystemInfoGroup> = Vec::new();
    for (key, mountpoints, usage, created) in merged {
        let separator = key.find('·').unwrap_or(0);
        let device = key[..separator].to_owned();
        let filesystem = key[separator + '·'.len_utf8()..]
            .rsplit_once('·')
            .map(|(filesystem, _)| filesystem.to_owned())
            .unwrap_or_else(|| "unknown".to_owned());
        let mut entries = vec![
            SystemInfoEntry::new("device", format!("{device} · {filesystem}")),
            SystemInfoEntry::new("mounts", mountpoints.join("、")),
            SystemInfoEntry::new("usage", usage),
        ];
        if let Some(created) = created {
            entries.push(SystemInfoEntry::new("created", format_iso_timestamp(&created)));
        }
        groups.push(SystemInfoGroup {
            title: None,
            entries,
        });
    }
    Ok(groups)
}

fn build_local_ip(section: &serde_json::Value) -> Result<Vec<SystemInfoGroup>, NormalizeError> {
    let Some(result) = result_obj(section) else {
        return Err(NormalizeError::NoObjectResult);
    };
    let Some(devices) = array_of(result) else {
        return Ok(Vec::new());
    };
    let mut rows: Vec<(Option<String>, String)> = Vec::new();
    for device in devices {
        let Some(ipv4) = string_of(device, "ipv4") else {
            continue;
        };
        rows.push((string_of(device, "name"), ipv4));
    }
    let groups = if rows.len() == 1 {
        let (name, ipv4) = rows.remove(0);
        let value = match name {
            Some(name) => format!("{name} · {ipv4}"),
            None => ipv4,
        };
        vec![SystemInfoGroup {
            title: None,
            entries: vec![SystemInfoEntry::new("address", value)],
        }]
    } else {
        rows.into_iter()
            .map(|(title, ipv4)| SystemInfoGroup {
                title,
                entries: vec![SystemInfoEntry::new("address", ipv4)],
            })
            .collect()
    };
    Ok(groups)
}

fn build_battery(section: &serde_json::Value) -> Result<Vec<SystemInfoGroup>, NormalizeError> {
    let Some(result) = result_obj(section) else {
        return Err(NormalizeError::NoObjectResult);
    };
    let Some(devices) = array_of(result) else {
        return Ok(Vec::new());
    };
    let mut rows: Vec<(Option<String>, SystemInfoEntry, SystemInfoEntry)> = Vec::new();
    for device in devices {
        let Some(capacity) = f64_of(device, "capacity") else {
            continue;
        };
        let capacity_value = join_parts(
            " · ",
            [
                Some(format!("{}%", capacity.round() as u64)),
                join_parts(", ", string_array_of(device, "status")),
            ]
            .into_iter()
            .flatten(),
        )
        .unwrap_or_else(|| "unknown".to_owned());
        let battery = join_parts(
            " · ",
            [
                join_parts(
                    " ",
                    [string_of(device, "manufacturer"), string_of(device, "modelName")]
                        .into_iter()
                        .flatten(),
                ),
                string_of(device, "technology"),
                u64_of(device, "cycleCount").map(|count| format!("{count} 次循环")),
            ]
            .into_iter()
            .flatten(),
        )
        .unwrap_or_else(|| "unknown".to_owned());
        rows.push((
            string_of(device, "modelName"),
            SystemInfoEntry::new("capacity", capacity_value),
            SystemInfoEntry::new("battery", battery),
        ));
    }
    let groups = if rows.len() == 1 {
        let (_, capacity, battery) = rows.remove(0);
        vec![SystemInfoGroup {
            title: None,
            entries: vec![capacity, battery],
        }]
    } else {
        rows.into_iter()
            .map(|(title, capacity, battery)| SystemInfoGroup {
                title,
                entries: vec![capacity, battery],
            })
            .collect()
    };
    Ok(groups)
}

fn build_locale(section: &serde_json::Value) -> Result<Vec<SystemInfoGroup>, NormalizeError> {
    let Some(result) = result_obj(section) else {
        return Err(NormalizeError::NoObjectResult);
    };
    let value = result.as_str().map(str::trim).filter(|value| !value.is_empty());
    Ok(single_group(
        value
            .into_iter()
            .map(|value| SystemInfoEntry::new("locale", value))
            .collect(),
    ))
}

fn single_group(entries: Vec<SystemInfoEntry>) -> Vec<SystemInfoGroup> {
    if entries.is_empty() {
        Vec::new()
    } else {
        vec![SystemInfoGroup {
            title: None,
            entries,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration as StdDuration;

    const FIXTURE: &str = r##"[
      {"type":"Separator","error":"Unsupported for JSON format"},
      {"type":"OS","result":{"buildID":"rolling","codename":"","id":"cachyos","idLike":"arch","name":"CachyOS Linux","prettyName":"CachyOS","variant":"","variantID":"","version":"rolling","versionID":""}},
      {"type":"Host","result":{"family":"ThinkBook 14 G8+ AGP","name":"21VB","version":"ThinkBook 14 G8+ AGP","sku":"LENOVO_MT_21VB","vendor":"LENOVO","serial":"","uuid":""}},
      {"type":"Kernel","result":{"architecture":"x86_64","name":"Linux","release":"7.1.6-1-cachyos","version":"#1 SMP PREEMPT_DYNAMIC Mon, 03 Aug 2026 16:03:35 +0000","pageSize":4096}},
      {"type":"Uptime","result":{"uptime":61739140,"bootTime":"2026-08-13T08:26:29.152+0800"}},
      {"type":"Packages","result":{"all":1788,"flatpakSystem":5,"pacman":1783}},
      {"type":"Shell","result":{"exe":"bash","exeName":"bash","exePath":"/usr/bin/bash","pid":1117743,"processName":"bash","prettyName":"bash","version":"5.3.15"}},
      {"type":"WM","result":{"processName":"niri","prettyName":"niri","protocolName":"Wayland","version":"26.04"}},
      {"type":"Display","result":[{"id":211213632613,"name":"MNE507ZA2-4","output":{"width":3072,"height":1920,"refreshRate":60.001,"dpi":168},"type":"Builtin","hdrStatus":"Supported"}]},
      {"type":"CPU","result":{"cpu":"AMD Ryzen AI 7 H 450","vendor":"AuthenticAMD","cores":{"physical":8,"logical":16,"online":16},"frequency":{"base":2005,"max":2000},"temperature":null,"march":"x86_64-v4","codeName":"Ryzen AI 7 (Krackan Point)","technology":"TSMC N4P"}},
      {"type":"GPU","result":[{"driver":"amdgpu","name":"AMD Radeon 860M Graphics","type":"Integrated","platformApi":"DRM (card1)"}]},
      {"type":"Memory","result":{"total":32752279552,"used":18332606464}},
      {"type":"Swap","result":[{"name":"/dev/zram0","used":5847838720,"total":32752267264}]},
      {"type":"Disk","result":[
        {"bytes":{"available":673516605440,"free":679165829120,"total":1023670538240,"used":344504709120},"filesystem":"btrfs","mountpoint":"/","mountFrom":"/dev/nvme0n1p2","name":"root","createTime":"2026-07-31T00:05:07.512+0800"},
        {"bytes":{"available":673516605440,"free":679165829120,"total":1023670538240,"used":344504709120},"filesystem":"btrfs","mountpoint":"/home","mountFrom":"/dev/nvme0n1p2","name":"root","createTime":"2026-07-31T00:05:07.512+0800"},
        {"bytes":{"available":673516605440,"free":679165829120,"total":1023670538240,"used":344504709120},"filesystem":"btrfs","mountpoint":"/root","mountFrom":"/dev/nvme0n1p2","name":"root","createTime":"2026-07-31T00:05:07.512+0800"},
        {"bytes":{"available":673516605440,"free":679165829120,"total":1023670538240,"used":344504709120},"filesystem":"btrfs","mountpoint":"/srv","mountFrom":"/dev/nvme0n1p2","name":"root","createTime":"2026-07-31T00:05:07.512+0800"},
        {"bytes":{"available":673516605440,"free":679165829120,"total":1023670538240,"used":344504709120},"filesystem":"btrfs","mountpoint":"/var/cache","mountFrom":"/dev/nvme0n1p2","name":"root","createTime":"2026-07-31T00:05:07.512+0800"},
        {"bytes":{"available":673516605440,"free":679165829120,"total":1023670538240,"used":344504709120},"filesystem":"btrfs","mountpoint":"/var/log","mountFrom":"/dev/nvme0n1p2","name":"root","createTime":"2026-07-31T00:05:07.512+0800"},
        {"bytes":{"available":673516605440,"free":679165829120,"total":1023670538240,"used":344504709120},"filesystem":"btrfs","mountpoint":"/var/tmp","mountFrom":"/dev/nvme0n1p2","name":"root","createTime":"2026-07-31T00:05:07.512+0800"}
      ]},
      {"type":"LocalIp","result":[{"name":"Mihomo","defaultRoute":{"ipv4":true},"ipv4":"198.18.0.1/30"}]},
      {"type":"Battery","result":[{"capacity":93.0,"manufacturer":"BYD","modelName":"L25B4PE3","technology":"Li-ion","cycleCount":7,"status":["AC Connected","Charging"]}]},
      {"type":"Locale","result":"zh_CN.UTF-8"}
    ]"##;

    fn entries(section: &SystemInfoSection) -> Vec<(String, String)> {
        section
            .groups
            .iter()
            .flat_map(|group| group.entries.iter().map(|entry| (entry.key.clone(), entry.value.clone())))
            .collect()
    }

    #[test]
    fn formatters_are_stable() {
        assert_eq!(format_bytes(4096), "4.0 KiB");
        assert_eq!(format_bytes(18332606464), "17.1 GiB");
        assert_eq!(format_bytes(1023670538240), "953.4 GiB");
        assert_eq!(format_percent(344504709120, 1023670538240), "34%");
        assert_eq!(format_percent(0, 0), "0%");
        assert_eq!(format_mhz(2005), "2.0 GHz");
        assert_eq!(format_mhz(500), "500 MHz");
        assert_eq!(format_uptime(61739140), "1 年 11 个月");
        assert_eq!(format_uptime(45), "45 秒");
        assert_eq!(format_uptime(0), "刚刚");
        assert_eq!(format_uptime(3600 * 24 * 3 + 7200), "3 天 2 小时");
        assert_eq!(
            format_iso_timestamp("2026-08-13T08:26:29.152+0800"),
            "2026-08-13 08:26:29 (+0800)"
        );
        assert_eq!(format_hz(60.001), "60 Hz");
    }

    #[test]
    fn normalize_matches_approved_facts() {
        let sections = normalize(FIXTURE.as_bytes()).expect("fixture parses");
        let ids: Vec<&str> = sections.iter().map(|section| section.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "OS", "Host", "Kernel", "Uptime", "Packages", "Shell", "WM", "Display", "CPU",
                "GPU", "Memory", "Swap", "Disk", "LocalIp", "Battery", "Locale"
            ]
        );

        let os = entries(&sections[0]);
        assert_eq!(os, vec![
            ("os_name".into(), "CachyOS Linux".into()),
            ("os_version".into(), "rolling · cachyos".into()),
        ]);

        let host = entries(&sections[1]);
        assert_eq!(host, vec![
            ("vendor".into(), "LENOVO".into()),
            ("model".into(), "ThinkBook 14 G8+ AGP (21VB)".into()),
        ]);

        let kernel = entries(&sections[2]);
        assert_eq!(kernel, vec![
            ("kernel".into(), "7.1.6-1-cachyos · x86_64".into()),
            ("build".into(), "#1 SMP PREEMPT_DYNAMIC Mon, 03 Aug 2026 16:03:35 +0000".into()),
        ]);

        let uptime = entries(&sections[3]);
        assert_eq!(uptime, vec![
            ("uptime".into(), "1 年 11 个月".into()),
            ("boot_time".into(), "2026-08-13 08:26:29 (+0800)".into()),
        ]);

        let packages = entries(&sections[4]);
        assert_eq!(packages, vec![(
            "packages".into(),
            "1783 pacman · 5 flatpak · 合计 1788".into(),
        )]);

        let shell = entries(&sections[5]);
        assert_eq!(shell, vec![("shell".into(), "bash · 5.3.15".into())]);

        let wm = entries(&sections[6]);
        assert_eq!(wm, vec![("wm".into(), "niri · 26.04 · Wayland".into())]);

        let display = entries(&sections[7]);
        assert_eq!(display, vec![(
            "display".into(),
            "MNE507ZA2-4 · 3072×1920 @ 60 Hz · dpi 168 · Builtin · HDR".into(),
        )]);

        let cpu = entries(&sections[8]);
        assert_eq!(cpu, vec![
            ("cpu_name".into(), "AMD Ryzen AI 7 H 450".into()),
            ("cores".into(), "8 物理 / 16 逻辑".into()),
            ("frequency".into(), "2.0 GHz · x86_64-v4".into()),
            ("codename".into(), "Ryzen AI 7 (Krackan Point) · TSMC N4P".into()),
        ]);

        let gpu = entries(&sections[9]);
        assert_eq!(gpu, vec![
            ("gpu".into(), "AMD Radeon 860M Graphics".into()),
            ("driver".into(), "amdgpu · Integrated · DRM (card1)".into()),
        ]);

        let memory = entries(&sections[10]);
        assert_eq!(memory, vec![("memory".into(), "17.1 GiB / 30.5 GiB (56%)".into())]);

        let swap = entries(&sections[11]);
        assert_eq!(swap, vec![(
            "swap".into(),
            "/dev/zram0 · 5.4 GiB / 30.5 GiB (18%)".into(),
        )]);

        let disk = entries(&sections[12]);
        assert_eq!(disk, vec![
            ("device".into(), "/dev/nvme0n1p2 · btrfs".into()),
            ("mounts".into(), "/、/home、/root、/srv、/var/cache、/var/log、/var/tmp".into()),
            ("usage".into(), "320.8 GiB / 953.4 GiB (34%) · 可用 627.3 GiB".into()),
            ("created".into(), "2026-07-31 00:05:07 (+0800)".into()),
        ]);

        let local_ip = entries(&sections[13]);
        assert_eq!(local_ip, vec![("address".into(), "Mihomo · 198.18.0.1/30".into())]);

        let battery = entries(&sections[14]);
        assert_eq!(battery, vec![
            ("capacity".into(), "93% · AC Connected, Charging".into()),
            ("battery".into(), "BYD L25B4PE3 · Li-ion · 7 次循环".into()),
        ]);

        let locale = entries(&sections[15]);
        assert_eq!(locale, vec![("locale".into(), "zh_CN.UTF-8".into())]);

        // Every section is a single group without a title on this host.
        for section in &sections {
            assert_eq!(section.groups.len(), 1);
            assert_eq!(section.groups[0].title, None);
        }
    }

    #[test]
    fn normalize_skips_error_and_missing_sections() {
        let input = br#"[
          {"type":"Separator","error":"Unsupported for JSON format"},
          {"type":"OS","result":{"prettyName":"CachyOS","version":"rolling","id":"cachyos"}},
          {"type":"WM","error":"No WM found"}
        ]"#;
        let sections = normalize(input).expect("parses");
        let ids: Vec<&str> = sections.iter().map(|section| section.id.as_str()).collect();
        assert_eq!(ids, ["OS"]);
    }

    #[test]
    fn normalize_rejects_non_array() {
        assert!(matches!(
            normalize(b"not json"),
            Err(NormalizeError::NotArray)
        ));
        assert!(matches!(
            normalize(b"{\"type\":\"OS\"}"),
            Err(NormalizeError::NotArray)
        ));
    }

    #[test]
    fn normalize_splits_multi_device_sections_into_groups() {
        let input = br#"[
          {"type":"GPU","result":[
            {"driver":"amdgpu","name":"AMD Radeon 860M Graphics","type":"Integrated"},
            {"driver":"nvidia","name":"NVIDIA RTX 4070","type":"Discrete"}
          ]},
          {"type":"Swap","result":[
            {"name":"/dev/zram0","used":100,"total":200},
            {"name":"/dev/zram1","used":300,"total":400}
          ]}
        ]"#;
        let sections = normalize(input).expect("parses");
        assert_eq!(sections.len(), 2);
        let gpu = &sections[0];
        assert_eq!(gpu.groups.len(), 2);
        assert_eq!(gpu.groups[0].title.as_deref(), Some("AMD Radeon 860M Graphics"));
        assert_eq!(gpu.groups[1].title.as_deref(), Some("NVIDIA RTX 4070"));
        let swap = &sections[1];
        assert_eq!(swap.groups.len(), 2);
        assert_eq!(swap.groups[0].title.as_deref(), Some("/dev/zram0"));
        assert_eq!(swap.groups[1].entries[0].value, "300 B / 400 B (75%)");
    }

    // --- collector tests with a fake fastfetch binary -----------------------

    fn fake_binary(script: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("fastfetch");
        std::fs::write(&path, script).expect("write script");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        dir
    }

    const FAKE_SCRIPT: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  echo "fastfetch 2.67.0 (x86_64)"
  exit 0
fi
cat <<'JSON'
[{"type":"OS","result":{"prettyName":"CachyOS Linux","version":"rolling","id":"cachyos"}}]
JSON
"#;

    #[tokio::test]
    async fn collect_with_fake_binary_is_healthy() {
        let dir = fake_binary(FAKE_SCRIPT);
        let outcome = SystemInfoCollector::default()
            .with_binary(dir.path().join("fastfetch"))
            .collect()
            .await;
        assert_eq!(outcome.status, SystemInfoStatus::Healthy);
        assert_eq!(outcome.reason, "fastfetch_ok");
        assert_eq!(outcome.tool_version.as_deref(), Some("fastfetch 2.67.0 (x86_64)"));
        assert_eq!(outcome.sections.len(), 1);
        assert_eq!(outcome.sections[0].id, "OS");
        assert!(outcome.captured_at_unix_ms.is_some());
    }

    #[tokio::test]
    async fn collect_with_missing_binary_is_unsupported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outcome = SystemInfoCollector::default()
            .with_binary(dir.path().join("does-not-exist"))
            .collect()
            .await;
        assert_eq!(outcome.status, SystemInfoStatus::Unsupported);
        assert_eq!(outcome.reason, "fastfetch_not_found");
        assert!(!outcome.retryable);
        assert!(outcome.sections.is_empty());
    }

    #[tokio::test]
    async fn collect_with_nonzero_exit_is_degraded() {
        let dir = fake_binary("#!/bin/sh\necho oops >&2\nexit 3\n");
        let outcome = SystemInfoCollector::default()
            .with_binary(dir.path().join("fastfetch"))
            .collect()
            .await;
        assert_eq!(outcome.status, SystemInfoStatus::Degraded);
        assert_eq!(outcome.reason, "fastfetch_exit_nonzero");
        assert!(outcome.retryable);
    }

    #[tokio::test]
    async fn collect_with_invalid_json_is_degraded() {
        let dir = fake_binary("#!/bin/sh\necho 'not json at all'\n");
        let outcome = SystemInfoCollector::default()
            .with_binary(dir.path().join("fastfetch"))
            .collect()
            .await;
        assert_eq!(outcome.status, SystemInfoStatus::Degraded);
        assert_eq!(outcome.reason, "fastfetch_output_invalid");
        assert!(outcome.retryable);
    }

    #[tokio::test]
    async fn collect_timeout_is_degraded_and_kills_child() {
        let dir = fake_binary("#!/bin/sh\nsleep 30\n");
        let outcome = SystemInfoCollector::default()
            .with_binary(dir.path().join("fastfetch"))
            .with_timeout(StdDuration::from_millis(300))
            .collect()
            .await;
        assert_eq!(outcome.status, SystemInfoStatus::Degraded);
        assert_eq!(outcome.reason, "fastfetch_timeout");
        assert!(outcome.retryable);
    }
}
