use crate::identity::resolve_grouping;
use localdesk_domain::{GroupingResolution, IssueCount, MetricState, MetricValue};
use localdesk_telemetry_helper_protocol::{
    HelperSnapshot, MAX_PROCESS_RECORDS, MAX_STRING_BYTES, PrivateApplicationResourceRecord,
    PrivateCgroupRecord, PrivateGroupingResolution, PrivateIssueCount, PrivateMetric,
    PrivateMetricState, PrivateProcessIdentity, PrivateProcessRecord, PrivateSnapshot,
    PrivateSystemFdSnapshot,
};
use nix::unistd::{SysconfVar, Uid, sysconf};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read},
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use uuid::Uuid;

const MAX_COMM_BYTES: usize = 256;
const MAX_CGROUP_BYTES: usize = MAX_STRING_BYTES;

#[derive(Debug, Error)]
pub enum ProcError {
    #[error("proc boot id is unavailable")]
    BootIdUnavailable,
    #[error("proc boot id is invalid")]
    BootIdInvalid,
    #[error("aggregate proc cpu stat is unavailable")]
    CpuStatUnavailable,
    #[error("aggregate proc cpu stat is invalid")]
    CpuStatInvalid,
    #[error("proc root cannot be read: {0}")]
    Root(#[source] io::Error),
    #[error("system page or clock size is unavailable")]
    SystemConfig,
    #[error("numeric process entry count exceeds {MAX_PROCESS_RECORDS}")]
    ProcessLimitExceeded,
    #[error("private telemetry snapshot is invalid: {0}")]
    SnapshotInvalid(String),
}

impl ProcError {
    pub const fn reason_code(&self) -> &'static str {
        match self {
            Self::BootIdUnavailable => "boot_id_unavailable",
            Self::BootIdInvalid => "boot_id_invalid",
            Self::CpuStatUnavailable => "cpu_stat_unavailable",
            Self::CpuStatInvalid => "cpu_stat_invalid",
            Self::Root(_) => "proc_root_unavailable",
            Self::SystemConfig => "system_config_unavailable",
            Self::ProcessLimitExceeded => "process_limit_exceeded",
            Self::SnapshotInvalid(_) => "snapshot_invalid",
        }
    }

    pub const fn retryable(&self) -> bool {
        !matches!(self, Self::SnapshotInvalid(_))
    }
}

#[derive(Debug, Clone, Error, Eq, PartialEq)]
pub enum StatParseError {
    #[error("process stat is missing a command name")]
    MissingCommand,
    #[error("process stat has no fields after command name")]
    MissingFields,
    #[error("process stat field is invalid: {0}")]
    InvalidField(&'static str),
    #[error("process stat rss is negative")]
    NegativeRss,
    #[error("process stat rss overflows the page size")]
    RssOverflow,
}

#[derive(Debug, Clone, Eq, Hash, PartialEq)]
pub(crate) struct ProcessIdentity {
    pub boot_id: String,
    pub pid: u32,
    pub start_time_ticks: u64,
    pub euid: u32,
}

impl ProcessIdentity {
    pub(crate) fn stable_key(&self) -> String {
        format!("{}:{}:{}", self.boot_id, self.pid, self.start_time_ticks)
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct ParsedStat {
    pub identity: ProcessIdentity,
    pub ppid: u32,
    pub comm: String,
    pub cpu_jiffies: u64,
    pub rss_bytes: Result<u64, StatParseError>,
}

pub(crate) fn parse_process_stat(
    content: &str,
    pid: u32,
    boot_id: &str,
    page_size: u64,
) -> Result<ParsedStat, StatParseError> {
    let open = content.find('(').ok_or(StatParseError::MissingCommand)?;
    let close = content.rfind(')').ok_or(StatParseError::MissingCommand)?;
    if close <= open {
        return Err(StatParseError::MissingCommand);
    }
    let comm = bound_comm(&content[open + 1..close]);
    let fields = content[close + 1..].split_whitespace().collect::<Vec<_>>();
    if fields.len() <= 21 {
        return Err(StatParseError::MissingFields);
    }

    let ppid = fields[1]
        .parse::<u32>()
        .map_err(|_| StatParseError::InvalidField("ppid"))?;
    let utime = fields[11]
        .parse::<u64>()
        .map_err(|_| StatParseError::InvalidField("utime"))?;
    let stime = fields[12]
        .parse::<u64>()
        .map_err(|_| StatParseError::InvalidField("stime"))?;
    let start_time_ticks = fields[19]
        .parse::<u64>()
        .map_err(|_| StatParseError::InvalidField("starttime"))?;
    let rss_pages = fields[21]
        .parse::<i64>()
        .map_err(|_| StatParseError::InvalidField("rss"))?;
    let cpu_jiffies = utime
        .checked_add(stime)
        .ok_or(StatParseError::InvalidField("cpu"))?;
    let rss_bytes = if rss_pages < 0 {
        Err(StatParseError::NegativeRss)
    } else {
        (rss_pages as u64)
            .checked_mul(page_size)
            .ok_or(StatParseError::RssOverflow)
    };

    Ok(ParsedStat {
        identity: ProcessIdentity {
            boot_id: boot_id.to_owned(),
            pid,
            start_time_ticks,
            euid: 0,
        },
        ppid,
        comm,
        cpu_jiffies,
        rss_bytes,
    })
}

#[derive(Debug, Clone)]
pub(crate) struct RawProcess {
    pub identity: ProcessIdentity,
    pub ppid: u32,
    pub comm: String,
    pub exe_basename: Option<String>,
    pub cgroup_content: String,
    pub application_key: String,
    pub desktop_entry_id: Option<String>,
    pub grouping_resolution: GroupingResolution,
    pub cpu_jiffies: u64,
    pub rss_bytes: MetricValue<u64>,
    pub pss_bytes: MetricValue<u64>,
    pub fd_used: MetricValue<u64>,
    pub fd_soft_limit: MetricValue<u64>,
    pub fd_percent_of_soft_limit: MetricValue<f64>,
}

#[derive(Debug, Clone)]
pub(crate) struct RawCgroupRecord {
    pub cgroup_path: String,
    pub application_key: String,
    pub cpu_usage_usec: MetricValue<u64>,
    pub memory_current_bytes: MetricValue<u64>,
    pub process_count: MetricValue<u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct RawSystemFdSnapshot {
    pub file_nr_allocated: MetricValue<u64>,
    pub file_nr_max: MetricValue<u64>,
    pub file_max: MetricValue<u64>,
    pub pressure_percent: MetricValue<f64>,
}

#[derive(Debug, Clone)]
pub(crate) struct RawApplicationResourceRecord {
    pub application_key: String,
    pub process_count: u64,
    pub proc_cpu_jiffies_sum: MetricValue<u64>,
    pub rss_sum_bytes: MetricValue<u64>,
    pub pss_sum_bytes: MetricValue<u64>,
    pub fd_used_sum: MetricValue<u64>,
    pub fd_soft_limit_sum: MetricValue<u64>,
    pub fd_percent_of_attributed_sum: MetricValue<f64>,
    pub fd_percent_of_soft_limit_sum: MetricValue<f64>,
    pub cgroup_cpu_usage_usec: MetricValue<u64>,
    pub memory_current_bytes: MetricValue<u64>,
    pub cgroup_process_count: MetricValue<u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct RawSnapshot {
    pub boot_id: String,
    pub euid: u32,
    pub captured_at_unix_ms: i64,
    pub captured_at_monotonic_ns: MetricValue<u64>,
    pub total_cpu_jiffies: u64,
    pub logical_cpu_count: u32,
    pub processes: Vec<RawProcess>,
    pub cgroups: Vec<RawCgroupRecord>,
    pub applications: Vec<RawApplicationResourceRecord>,
    pub system_fd: RawSystemFdSnapshot,
    pub excluded_other_uid: u64,
    pub skipped_race: u64,
    pub permission_denied_counts: Vec<IssueCount>,
    pub issues: Vec<IssueCount>,
}

type RevalidationHook = Arc<dyn Fn(&Path) + Send + Sync>;

#[derive(Clone)]
pub struct ProcCollector {
    proc_root: PathBuf,
    cgroup_root: PathBuf,
    desktop_roots: Vec<PathBuf>,
    euid: u32,
    page_size: u64,
    _clk_tck: u64,
    identity_salt: u128,
    revalidation_hook: Option<RevalidationHook>,
}

impl ProcCollector {
    pub fn new(proc_root: impl AsRef<Path>) -> Result<Self, ProcError> {
        let page_size = sysconf(SysconfVar::PAGE_SIZE)
            .ok()
            .flatten()
            .and_then(|value| u64::try_from(value).ok())
            .filter(|value| *value > 0)
            .ok_or(ProcError::SystemConfig)?;
        let clk_tck = sysconf(SysconfVar::CLK_TCK)
            .ok()
            .flatten()
            .and_then(|value| u64::try_from(value).ok())
            .filter(|value| *value > 0)
            .ok_or(ProcError::SystemConfig)?;
        Ok(Self::with_config(
            proc_root,
            Uid::effective().as_raw(),
            page_size,
            clk_tck,
            desktop_roots_from_env(),
        ))
    }

    pub fn with_config(
        proc_root: impl AsRef<Path>,
        euid: u32,
        page_size: u64,
        clk_tck: u64,
        desktop_roots: Vec<PathBuf>,
    ) -> Self {
        Self::with_config_and_cgroup_root(
            proc_root,
            "/sys/fs/cgroup",
            euid,
            page_size,
            clk_tck,
            desktop_roots,
        )
    }

    pub fn with_config_and_cgroup_root(
        proc_root: impl AsRef<Path>,
        cgroup_root: impl AsRef<Path>,
        euid: u32,
        page_size: u64,
        clk_tck: u64,
        desktop_roots: Vec<PathBuf>,
    ) -> Self {
        Self {
            proc_root: proc_root.as_ref().to_owned(),
            cgroup_root: cgroup_root.as_ref().to_owned(),
            desktop_roots,
            euid,
            page_size,
            _clk_tck: clk_tck,
            identity_salt: Uuid::new_v4().as_u128(),
            revalidation_hook: None,
        }
    }

    pub fn with_revalidation_hook<F>(mut self, hook: F) -> Self
    where
        F: Fn(&Path) + Send + Sync + 'static,
    {
        self.revalidation_hook = Some(Arc::new(hook));
        self
    }

    pub fn proc_root(&self) -> &Path {
        &self.proc_root
    }

    pub fn collect_protocol(&self) -> Result<HelperSnapshot, ProcError> {
        let raw = self.collect_raw()?;
        raw.into_protocol()
    }

    pub(crate) fn collect_raw(&self) -> Result<RawSnapshot, ProcError> {
        let boot_id = read_boot_id(&self.proc_root)?;
        let (total_cpu_jiffies, logical_cpu_count) = read_cpu_stat(&self.proc_root)?;
        let mut processes = Vec::new();
        let mut excluded_other_uid = 0_u64;
        let mut skipped_race = 0_u64;
        let mut permission_denied_counts = Vec::new();
        let mut issues = Vec::new();
        let mut numeric_entries = 0_usize;

        let entries = fs::read_dir(&self.proc_root).map_err(ProcError::Root)?;
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    let code = if error.kind() == io::ErrorKind::PermissionDenied {
                        "process_dir_permission_denied"
                    } else {
                        "process_dir_read_error"
                    };
                    add_issue(&mut issues, code);
                    if code.ends_with("permission_denied") {
                        add_issue(&mut permission_denied_counts, code);
                    }
                    continue;
                }
            };
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Ok(pid) = name.parse::<u32>() else {
                continue;
            };
            if pid == std::process::id() {
                continue;
            }
            numeric_entries = numeric_entries.saturating_add(1);
            if numeric_entries > MAX_PROCESS_RECORDS {
                return Err(ProcError::ProcessLimitExceeded);
            }

            match self.read_process(pid, &boot_id) {
                Ok(ProcessOutcome::Included(mut process)) => {
                    process.cgroup_content = process.cgroup_content.trim().to_owned();
                    record_metric_issue(
                        &process.pss_bytes,
                        &mut issues,
                        &mut permission_denied_counts,
                    );
                    processes.push(*process);
                }
                Ok(ProcessOutcome::Excluded) => excluded_other_uid += 1,
                Err(error) => {
                    if matches!(error, ProcReadError::Race) {
                        skipped_race = skipped_race.saturating_add(1);
                    }
                    add_issue(&mut issues, error.reason_code());
                    if matches!(error, ProcReadError::Permission) {
                        add_issue(&mut permission_denied_counts, error.reason_code());
                    }
                }
            }
        }

        resolve_grouping(&mut processes, self.identity_salt, &self.desktop_roots);
        let cgroups = collect_cgroups(
            &self.cgroup_root,
            &processes,
            &mut issues,
            &mut permission_denied_counts,
        );
        let system_fd = read_system_fd(&self.proc_root);
        let applications = aggregate_private_applications(&processes, &cgroups, &mut issues);
        record_metric_issue(
            &system_fd.file_nr_allocated,
            &mut issues,
            &mut permission_denied_counts,
        );
        record_metric_issue(
            &system_fd.file_nr_max,
            &mut issues,
            &mut permission_denied_counts,
        );
        record_metric_issue(
            &system_fd.file_max,
            &mut issues,
            &mut permission_denied_counts,
        );
        let captured_at_monotonic_ns = read_monotonic_ns(&self.proc_root);
        record_metric_issue(
            &captured_at_monotonic_ns,
            &mut issues,
            &mut permission_denied_counts,
        );
        Ok(RawSnapshot {
            boot_id,
            euid: self.euid,
            captured_at_unix_ms: unix_ms(),
            captured_at_monotonic_ns,
            total_cpu_jiffies,
            logical_cpu_count,
            processes,
            cgroups,
            applications,
            system_fd,
            excluded_other_uid,
            skipped_race,
            permission_denied_counts,
            issues,
        })
    }

    fn read_process(&self, pid: u32, boot_id: &str) -> Result<ProcessOutcome, ProcReadError> {
        let process_root = self.proc_root.join(pid.to_string());
        let stat_content = fs::read_to_string(process_root.join("stat")).map_err(classify_io)?;
        let stat = parse_process_stat(&stat_content, pid, boot_id, self.page_size)
            .map_err(ProcReadError::Stat)?;

        let status = fs::read_to_string(process_root.join("status")).map_err(classify_io)?;
        let (_real_uid, effective_uid) = parse_uids(&status).ok_or(ProcReadError::Status)?;
        if effective_uid != self.euid {
            return Ok(ProcessOutcome::Excluded);
        }

        let rss_bytes = match stat.rss_bytes {
            Ok(value) => MetricValue::known(value),
            Err(error) => MetricValue::unavailable(
                MetricState::Unknown,
                match error {
                    StatParseError::NegativeRss => "negative_rss",
                    StatParseError::RssOverflow => "rss_overflow",
                    _ => "rss_unknown",
                },
            ),
        };
        let pss_bytes = read_pss_bytes(&process_root.join("smaps_rollup"));
        let cgroup_content =
            fs::read_to_string(process_root.join("cgroup")).map_err(classify_io)?;
        if cgroup_content.len() > MAX_CGROUP_BYTES {
            return Err(ProcReadError::Oversized("cgroup"));
        }
        let exe_basename = read_executable_basename(&process_root);
        let fd_used = read_fd_count(&process_root.join("fd"));
        let fd_soft_limit = read_fd_soft_limit(&process_root.join("limits"));
        let fd_percent_of_soft_limit = derive_fd_percent(&fd_used, &fd_soft_limit);

        if let Some(hook) = &self.revalidation_hook {
            hook(&process_root);
        }
        let final_stat_content =
            fs::read_to_string(process_root.join("stat")).map_err(classify_io)?;
        let final_stat = parse_process_stat(&final_stat_content, pid, boot_id, self.page_size)
            .map_err(|_| ProcReadError::Race)?;
        let final_status = fs::read_to_string(process_root.join("status")).map_err(classify_io)?;
        let Some((_final_real_uid, final_effective_uid)) = parse_uids(&final_status) else {
            return Err(ProcReadError::Race);
        };
        if final_stat.identity.pid != stat.identity.pid
            || final_stat.identity.start_time_ticks != stat.identity.start_time_ticks
            || final_effective_uid != effective_uid
            || final_effective_uid != self.euid
        {
            return Err(ProcReadError::Race);
        }

        let identity = ProcessIdentity {
            euid: effective_uid,
            ..stat.identity
        };
        Ok(ProcessOutcome::Included(Box::new(RawProcess {
            identity,
            ppid: stat.ppid,
            comm: stat.comm,
            exe_basename,
            cgroup_content,
            application_key: String::new(),
            desktop_entry_id: None,
            grouping_resolution: GroupingResolution::Unknown,
            cpu_jiffies: stat.cpu_jiffies,
            rss_bytes,
            pss_bytes,
            fd_used,
            fd_soft_limit,
            fd_percent_of_soft_limit,
        })))
    }
}

#[derive(Debug)]
enum ProcessOutcome {
    Included(Box<RawProcess>),
    Excluded,
}

#[derive(Debug, Error)]
enum ProcReadError {
    #[error("process stat was invalid")]
    Stat(#[source] StatParseError),
    #[error("process status was invalid")]
    Status,
    #[error("process entry raced")]
    Race,
    #[error("process entry permission denied")]
    Permission,
    #[error("process detail is oversized: {0}")]
    Oversized(&'static str),
    #[error("process entry could not be read")]
    Other,
}

impl ProcReadError {
    const fn reason_code(&self) -> &'static str {
        match self {
            Self::Stat(_) => "process_stat_invalid",
            Self::Status => "process_status_invalid",
            Self::Race => "process_raced",
            Self::Permission => "process_permission_denied",
            Self::Oversized(_) => "process_detail_oversized",
            Self::Other => "process_read_error",
        }
    }
}

fn read_boot_id(proc_root: &Path) -> Result<String, ProcError> {
    let content = fs::read_to_string(proc_root.join("sys/kernel/random/boot_id"))
        .map_err(|_| ProcError::BootIdUnavailable)?;
    let boot_id = content.trim();
    if boot_id.is_empty() || boot_id.len() > 128 || boot_id.chars().any(char::is_whitespace) {
        return Err(ProcError::BootIdInvalid);
    }
    Ok(boot_id.to_owned())
}

fn read_cpu_stat(proc_root: &Path) -> Result<(u64, u32), ProcError> {
    let content =
        fs::read_to_string(proc_root.join("stat")).map_err(|_| ProcError::CpuStatUnavailable)?;
    let mut aggregate = None;
    let mut logical = 0_u32;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("cpu ") {
            let fields = rest.split_whitespace().take(8).collect::<Vec<_>>();
            if fields.len() < 8 {
                return Err(ProcError::CpuStatInvalid);
            }
            let mut total = 0_u64;
            for field in fields {
                let value = field
                    .parse::<u64>()
                    .map_err(|_| ProcError::CpuStatInvalid)?;
                total = total.checked_add(value).ok_or(ProcError::CpuStatInvalid)?;
            }
            aggregate = Some(total);
        } else if let Some(label) = line.split_whitespace().next()
            && let Some(suffix) = label.strip_prefix("cpu")
            && !suffix.is_empty()
            && suffix.bytes().all(|byte| byte.is_ascii_digit())
        {
            logical = logical.saturating_add(1);
        }
    }
    aggregate
        .map(|total| (total, logical.max(1)))
        .ok_or(ProcError::CpuStatInvalid)
}

fn parse_uids(status: &str) -> Option<(u32, u32)> {
    status.lines().find_map(|line| {
        let values = line
            .strip_prefix("Uid:")?
            .split_whitespace()
            .collect::<Vec<_>>();
        if values.len() < 2 {
            return None;
        }
        Some((values[0].parse().ok()?, values[1].parse().ok()?))
    })
}

fn read_fd_count(path: &Path) -> MetricValue<u64> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) => return metric_for_io(error, "fd"),
    };
    let mut count = 0_u64;
    for entry in entries {
        match entry {
            Ok(_) => count = count.saturating_add(1),
            Err(error) => return metric_for_io(error, "fd"),
        }
    }
    MetricValue::known(count)
}

fn read_executable_basename(process_root: &Path) -> Option<String> {
    fs::read_link(process_root.join("exe"))
        .ok()
        .and_then(|path| bounded_path_basename(&path))
        .or_else(|| read_argv0_basename(&process_root.join("cmdline")))
}

fn read_argv0_basename(path: &Path) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let mut bytes = [0_u8; MAX_STRING_BYTES];
    let count = file.read(&mut bytes).ok()?;
    let end = bytes[..count].iter().position(|byte| *byte == 0)?;
    let argv0 = String::from_utf8_lossy(&bytes[..end]);
    bounded_path_basename(Path::new(argv0.as_ref()))
}

fn bounded_path_basename(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_string_lossy();
    let bounded = bound_comm(&name);
    (!bounded.is_empty()).then_some(bounded)
}

fn read_pss_bytes(path: &Path) -> MetricValue<u64> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) => return metric_for_stable_io(error, "pss"),
    };
    let Some(line) = content.lines().find(|line| line.starts_with("Pss:")) else {
        return MetricValue::unavailable(MetricState::Unknown, "pss_missing");
    };
    let fields = line.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 3 || fields[2] != "kB" {
        return MetricValue::unavailable(MetricState::Unknown, "pss_invalid");
    }
    match fields[1]
        .parse::<u64>()
        .ok()
        .and_then(|value| value.checked_mul(1024))
    {
        Some(value) => MetricValue::known(value),
        None => MetricValue::unavailable(MetricState::Unknown, "pss_invalid"),
    }
}

fn collect_cgroups(
    cgroup_root: &Path,
    processes: &[RawProcess],
    issues: &mut Vec<IssueCount>,
    permission_denied_counts: &mut Vec<IssueCount>,
) -> Vec<RawCgroupRecord> {
    let mut scopes = BTreeMap::<String, String>::new();
    for process in processes {
        if let Some(path) = cgroup_v2_scope(&process.cgroup_content) {
            scopes.entry(path.clone()).or_insert_with(|| {
                // Prefer a desktop-resolved member key so a transient scope
                // (e.g. systemd `run-<pid>-i<id>.scope`) that hosts children
                // of a desktop-launched app is bound to that app.
                processes
                    .iter()
                    .find(|member| {
                        cgroup_v2_scope(&member.cgroup_content).as_deref() == Some(path.as_str())
                            && member.desktop_entry_id.is_some()
                    })
                    .map(|member| member.application_key.clone())
                    .unwrap_or_else(|| process.application_key.clone())
            });
        }
    }
    scopes
        .into_iter()
        .map(|(cgroup_path, application_key)| {
            let root = cgroup_path_on_disk(cgroup_root, &cgroup_path);
            let cpu_usage_usec = read_cpu_usage_usec(&root.join("cpu.stat"));
            let memory_current_bytes =
                read_u64_metric(&root.join("memory.current"), "cgroup_memory_current");
            let process_count = read_line_count(&root.join("cgroup.procs"), "cgroup_process_count");
            for metric in [&cpu_usage_usec, &memory_current_bytes, &process_count] {
                record_metric_issue(metric, issues, permission_denied_counts);
            }
            RawCgroupRecord {
                cgroup_path,
                application_key,
                cpu_usage_usec,
                memory_current_bytes,
                process_count,
            }
        })
        .collect()
}

fn cgroup_v2_scope(content: &str) -> Option<String> {
    content.lines().find_map(|line| {
        let path = line.strip_prefix("0::")?.trim();
        let scope = cgroup_scope(line)?;
        (path == scope).then_some(scope)
    })
}

fn cgroup_path_on_disk(root: &Path, cgroup_path: &str) -> PathBuf {
    let mut result = root.to_owned();
    for component in Path::new(cgroup_path).components() {
        if let Component::Normal(component) = component {
            result.push(component);
        }
    }
    result
}

fn read_cpu_usage_usec(path: &Path) -> MetricValue<u64> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) => return metric_for_io(error, "cgroup_cpu_usage"),
    };
    let Some(value) = content.lines().find_map(|line| {
        let value = line.strip_prefix("usage_usec ")?;
        value.trim().parse::<u64>().ok()
    }) else {
        return MetricValue::unavailable(MetricState::Unknown, "cgroup_cpu_usage_invalid");
    };
    MetricValue::known(value)
}

fn read_u64_metric(path: &Path, name: &str) -> MetricValue<u64> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) => return metric_for_io(error, name),
    };
    match content.trim().parse::<u64>() {
        Ok(value) => MetricValue::known(value),
        Err(_) => MetricValue::unavailable(MetricState::Unknown, format!("{name}_invalid")),
    }
}

fn read_line_count(path: &Path, name: &str) -> MetricValue<u64> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) => return metric_for_io(error, name),
    };
    let mut count = 0_u64;
    for line in content.lines() {
        if line.trim().parse::<u32>().is_err() {
            return MetricValue::unavailable(MetricState::Unknown, format!("{name}_invalid"));
        }
        count = count.saturating_add(1);
    }
    MetricValue::known(count)
}

fn aggregate_private_applications(
    processes: &[RawProcess],
    cgroups: &[RawCgroupRecord],
    issues: &mut Vec<IssueCount>,
) -> Vec<RawApplicationResourceRecord> {
    let mut attributed_fd_total = 0_u64;
    let mut attributed_fd_skipped = 0_u64;
    for process in processes {
        match process.fd_used.value {
            Some(value) => attributed_fd_total = attributed_fd_total.saturating_add(value),
            None => attributed_fd_skipped = attributed_fd_skipped.saturating_add(1),
        }
    }
    if attributed_fd_skipped > 0 {
        add_issue(issues, "attributed_fd_partial");
    }
    let attributed_fd_total = if attributed_fd_total > 0 || attributed_fd_skipped == 0 {
        MetricValue::known(attributed_fd_total)
    } else {
        MetricValue::unavailable(MetricState::Unknown, "attributed_fd_total_unknown")
    };
    let mut groups = BTreeMap::<String, Vec<&RawProcess>>::new();
    for process in processes {
        groups
            .entry(process.application_key.clone())
            .or_default()
            .push(process);
    }
    groups
        .into_iter()
        .map(|(application_key, members)| {
            let proc_cpu_jiffies_sum = checked_sum_u64(
                members.iter().map(|process| process.cpu_jiffies),
                "application_cpu_counter_overflow",
            );
            let rss_sum_bytes = sum_u64_metrics(
                members.iter().map(|process| &process.rss_bytes),
                "application_rss_unknown",
            );
            let pss_sum_bytes = sum_u64_metrics(
                members.iter().map(|process| &process.pss_bytes),
                "application_pss_unknown",
            );
            let fd_used_sum = sum_u64_metrics(
                members.iter().map(|process| &process.fd_used),
                "application_fd_used_unknown",
            );
            let fd_soft_limit_sum = sum_u64_metrics(
                members.iter().map(|process| &process.fd_soft_limit),
                "application_fd_limit_unknown",
            );
            let app_cgroups = cgroups
                .iter()
                .filter(|cgroup| cgroup.application_key == application_key)
                .collect::<Vec<_>>();
            let cgroup_cpu_usage_usec = sum_cgroup_metric(
                &app_cgroups,
                |cgroup| &cgroup.cpu_usage_usec,
                "application_cgroup_cpu_unknown",
            );
            let memory_current_bytes = sum_cgroup_metric(
                &app_cgroups,
                |cgroup| &cgroup.memory_current_bytes,
                "application_memory_current_unknown",
            );
            let cgroup_process_count = sum_cgroup_metric(
                &app_cgroups,
                |cgroup| &cgroup.process_count,
                "application_cgroup_process_count_unknown",
            );
            RawApplicationResourceRecord {
                application_key,
                process_count: members.len() as u64,
                proc_cpu_jiffies_sum,
                rss_sum_bytes,
                pss_sum_bytes,
                fd_percent_of_attributed_sum: derive_percent(
                    &fd_used_sum,
                    &attributed_fd_total,
                    "application_fd_attributed_percent_unknown",
                ),
                fd_percent_of_soft_limit_sum: derive_percent(
                    &fd_used_sum,
                    &fd_soft_limit_sum,
                    "application_fd_limit_percent_unknown",
                ),
                fd_used_sum,
                fd_soft_limit_sum,
                cgroup_cpu_usage_usec,
                memory_current_bytes,
                cgroup_process_count,
            }
        })
        .collect()
}

fn checked_sum_u64<I>(values: I, reason: &str) -> MetricValue<u64>
where
    I: Iterator<Item = u64>,
{
    let mut total = 0_u64;
    for value in values {
        let Some(next) = total.checked_add(value) else {
            return MetricValue::unavailable(MetricState::Unknown, reason);
        };
        total = next;
    }
    MetricValue::known(total)
}

fn sum_u64_metrics<'a, I>(metrics: I, reason: &str) -> MetricValue<u64>
where
    I: Iterator<Item = &'a MetricValue<u64>>,
{
    let metrics = metrics.collect::<Vec<_>>();
    if metrics.iter().any(|metric| !metric.is_known()) {
        return MetricValue::unavailable(derived_metric_state_slice(&metrics), reason);
    }
    checked_sum_u64(metrics.iter().filter_map(|metric| metric.value), reason)
}

fn sum_cgroup_metric<F>(cgroups: &[&RawCgroupRecord], select: F, reason: &str) -> MetricValue<u64>
where
    F: Fn(&RawCgroupRecord) -> &MetricValue<u64>,
{
    if cgroups.is_empty() {
        return MetricValue::unavailable(MetricState::Unknown, reason);
    }
    sum_u64_metrics(cgroups.iter().map(|cgroup| select(cgroup)), reason)
}

fn read_system_fd(proc_root: &Path) -> RawSystemFdSnapshot {
    let file_nr_path = proc_root.join("sys/fs/file-nr");
    let (file_nr_allocated, file_nr_max) = match fs::read_to_string(&file_nr_path) {
        Ok(content) => parse_file_nr(&content),
        Err(error) => {
            let metric = metric_for_stable_io(error, "file_nr");
            (metric.clone(), metric)
        }
    };
    let file_max = read_u64_stable_metric(&proc_root.join("sys/fs/file-max"), "file_max");
    let pressure_percent =
        derive_percent(&file_nr_allocated, &file_max, "system_fd_pressure_unknown");
    RawSystemFdSnapshot {
        file_nr_allocated,
        file_nr_max,
        file_max,
        pressure_percent,
    }
}

fn read_u64_stable_metric(path: &Path, name: &str) -> MetricValue<u64> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) => return metric_for_stable_io(error, name),
    };
    match content.trim().parse::<u64>() {
        Ok(value) => MetricValue::known(value),
        Err(_) => MetricValue::unavailable(MetricState::Unknown, format!("{name}_invalid")),
    }
}

fn parse_file_nr(content: &str) -> (MetricValue<u64>, MetricValue<u64>) {
    let fields = content.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 3 {
        let metric = MetricValue::unavailable(MetricState::Unknown, "file_nr_invalid");
        return (metric.clone(), metric);
    }
    let allocated = fields[0].parse::<u64>();
    let maximum = fields[2].parse::<u64>();
    match (allocated, maximum) {
        (Ok(allocated), Ok(maximum)) => {
            (MetricValue::known(allocated), MetricValue::known(maximum))
        }
        _ => {
            let metric = MetricValue::unavailable(MetricState::Unknown, "file_nr_invalid");
            (metric.clone(), metric)
        }
    }
}

fn derive_percent(
    numerator: &MetricValue<u64>,
    denominator: &MetricValue<u64>,
    reason: &str,
) -> MetricValue<f64> {
    match (numerator.value, denominator.value) {
        (Some(numerator), Some(denominator)) if denominator > 0 => {
            MetricValue::known(numerator as f64 * 100.0 / denominator as f64)
        }
        _ => MetricValue::unavailable(derived_metric_state([numerator, denominator]), reason),
    }
}

fn derived_metric_state<const N: usize>(metrics: [&MetricValue<u64>; N]) -> MetricState {
    derived_metric_state_slice(&metrics)
}

fn derived_metric_state_slice(metrics: &[&MetricValue<u64>]) -> MetricState {
    for state in [
        MetricState::PermissionDenied,
        MetricState::Raced,
        MetricState::Unbounded,
    ] {
        if metrics.iter().any(|metric| metric.state == state) {
            return state;
        }
    }
    MetricState::Unknown
}

fn record_metric_issue<T>(
    metric: &MetricValue<T>,
    issues: &mut Vec<IssueCount>,
    permission_denied_counts: &mut Vec<IssueCount>,
) {
    if metric.state == MetricState::Known {
        return;
    }
    let reason = metric.reason.as_deref().unwrap_or("metric_unknown");
    add_issue(issues, reason);
    if metric.state == MetricState::PermissionDenied {
        add_issue(permission_denied_counts, reason);
    }
}

fn read_fd_soft_limit(path: &Path) -> MetricValue<u64> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) => return metric_for_io(error, "fd_limit"),
    };
    let Some(line) = content
        .lines()
        .find(|line| line.trim_start().starts_with("Max open files"))
    else {
        return MetricValue::unavailable(MetricState::Unknown, "fd_limit_missing");
    };
    let fields = line.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 5 {
        return MetricValue::unavailable(MetricState::Unknown, "fd_limit_invalid");
    }
    if fields[3] == "unlimited" {
        MetricValue::unavailable(MetricState::Unbounded, "fd_limit_unlimited")
    } else {
        match fields[3].parse::<u64>() {
            Ok(0) => MetricValue::unavailable(MetricState::Unknown, "fd_limit_zero"),
            Ok(value) => MetricValue::known(value),
            Err(_) => MetricValue::unavailable(MetricState::Unknown, "fd_limit_invalid"),
        }
    }
}

fn derive_fd_percent(used: &MetricValue<u64>, limit: &MetricValue<u64>) -> MetricValue<f64> {
    match (used.value, limit.value) {
        (Some(used), Some(limit)) if used <= limit && limit > 0 => {
            MetricValue::known((used as f64) * 100.0 / (limit as f64))
        }
        (_, _)
            if used.state == MetricState::PermissionDenied
                || limit.state == MetricState::PermissionDenied =>
        {
            MetricValue::unavailable(
                MetricState::PermissionDenied,
                "fd_percent_permission_denied",
            )
        }
        (_, _) if limit.state == MetricState::Unbounded => {
            MetricValue::unavailable(MetricState::Unbounded, "fd_limit_unlimited")
        }
        (_, _) if used.state == MetricState::Raced || limit.state == MetricState::Raced => {
            MetricValue::unavailable(MetricState::Raced, "fd_percent_raced")
        }
        _ => MetricValue::unavailable(MetricState::Unknown, "fd_percent_unknown"),
    }
}

fn metric_for_io<T>(error: io::Error, metric: &str) -> MetricValue<T> {
    let state = match error.kind() {
        io::ErrorKind::PermissionDenied => MetricState::PermissionDenied,
        io::ErrorKind::NotFound => MetricState::Raced,
        _ => MetricState::Unknown,
    };
    let reason = match state {
        MetricState::PermissionDenied => format!("{metric}_permission_denied"),
        MetricState::Raced => format!("{metric}_raced"),
        _ => format!("{metric}_unknown"),
    };
    MetricValue::unavailable(state, reason)
}

fn metric_for_stable_io<T>(error: io::Error, metric: &str) -> MetricValue<T> {
    let state = if error.kind() == io::ErrorKind::PermissionDenied {
        MetricState::PermissionDenied
    } else {
        MetricState::Unknown
    };
    let reason = if state == MetricState::PermissionDenied {
        format!("{metric}_permission_denied")
    } else {
        format!("{metric}_unknown")
    };
    MetricValue::unavailable(state, reason)
}

fn classify_io(error: io::Error) -> ProcReadError {
    match error.kind() {
        io::ErrorKind::PermissionDenied => ProcReadError::Permission,
        io::ErrorKind::NotFound => ProcReadError::Race,
        _ => ProcReadError::Other,
    }
}

fn bound_comm(comm: &str) -> String {
    let mut result = comm.chars().take(MAX_COMM_BYTES).collect::<String>();
    result.retain(|ch| ch != '\0' && ch != '\n' && ch != '\r');
    result
}

pub(crate) fn desktop_roots_from_env() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(data_home) = std::env::var_os("XDG_DATA_HOME") {
        roots.push(PathBuf::from(data_home).join("applications"));
    } else if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".local/share/applications"));
    }
    let data_dirs = std::env::var_os("XDG_DATA_DIRS")
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_owned());
    roots.extend(
        data_dirs
            .split(':')
            .filter(|dir| !dir.is_empty())
            .map(|dir| PathBuf::from(dir).join("applications")),
    );
    roots
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn read_monotonic_ns(proc_root: &Path) -> MetricValue<u64> {
    let content = match fs::read_to_string(proc_root.join("uptime")) {
        Ok(content) => content,
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            return MetricValue::unavailable(
                MetricState::PermissionDenied,
                "proc_uptime_permission_denied",
            );
        }
        Err(_) => {
            return MetricValue::unavailable(MetricState::Unknown, "proc_uptime_unavailable");
        }
    };
    let Some(value) = content.split_whitespace().next() else {
        return MetricValue::unavailable(MetricState::Unknown, "proc_uptime_invalid");
    };
    let (seconds, fraction) = value.split_once('.').unwrap_or((value, ""));
    let Some(seconds_ns) = seconds
        .parse::<u64>()
        .ok()
        .and_then(|seconds| seconds.checked_mul(1_000_000_000))
    else {
        return MetricValue::unavailable(MetricState::Unknown, "proc_uptime_invalid");
    };
    if !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return MetricValue::unavailable(MetricState::Unknown, "proc_uptime_invalid");
    }
    let mut fraction_ns = 0_u64;
    for byte in fraction.bytes().take(9) {
        fraction_ns = fraction_ns * 10 + u64::from(byte - b'0');
    }
    for _ in fraction.len().min(9)..9 {
        fraction_ns *= 10;
    }
    seconds_ns
        .checked_add(fraction_ns)
        .map(MetricValue::known)
        .unwrap_or_else(|| MetricValue::unavailable(MetricState::Unknown, "proc_uptime_invalid"))
}

pub(crate) fn cgroup_scope(content: &str) -> Option<String> {
    content.lines().find_map(|line| {
        let (_, path) = line.rsplit_once(':')?;
        let path = path.trim();
        let component = path.trim_end_matches('/').rsplit('/').next()?;
        (component.ends_with(".scope")).then(|| path.to_owned())
    })
}

pub(crate) fn scope_label(scope: &str) -> String {
    let component = scope
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(scope);
    component
        .strip_prefix("app-")
        .unwrap_or(component)
        .strip_suffix(".scope")
        .unwrap_or(component)
        .to_owned()
}

pub(crate) fn desktop_id_for_scope(scope: &str, desktop_roots: &[PathBuf]) -> Option<String> {
    let label = scope_label(scope);
    if label.is_empty() {
        return None;
    }
    desktop_roots.iter().find_map(|root| {
        desktop_name_candidates(&label)
            .into_iter()
            .find_map(|candidate| {
                let path = root.join(format!("{candidate}.desktop"));
                path.is_file().then(|| {
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| candidate)
                })
            })
    })
}

/// Candidate desktop-file base names for a systemd scope label, most specific
/// first. systemd names runtime app scopes `app-<id>.scope`,
/// `app-<id>-<pid>.scope` (PID suffix when the same id is launched again) or
/// `app-<id>@<instance>.scope` (template instances), so a label such as
/// `codex-desktop-970898` must try `codex-desktop-970898` and then
/// `codex-desktop` before giving up.
fn desktop_name_candidates(label: &str) -> Vec<String> {
    fn stripped(label: &str) -> Vec<String> {
        let mut candidates = Vec::new();
        let mut current = label;
        loop {
            candidates.push(current.to_owned());
            let Some((base, suffix)) = current.rsplit_once('-') else {
                break;
            };
            if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
                break;
            }
            current = base;
        }
        candidates
    }
    let mut candidates = stripped(label);
    if let Some((base, _)) = label.rsplit_once('@') {
        for candidate in stripped(base) {
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
    }
    candidates
}

const DESKTOP_ENTRY_MAX_READ_BYTES: u64 = 16 * 1024;
const DESKTOP_ENTRY_NAME_MAX_CHARS: usize = 128;

/// The display name (`Name=` of the `[Desktop Entry]` section) of a matched
/// desktop entry, bounded and locale-independent.
pub(crate) fn desktop_display_name(desktop_id: &str, desktop_roots: &[PathBuf]) -> Option<String> {
    for root in desktop_roots {
        let path = root.join(desktop_id);
        if !path.is_file() {
            continue;
        }
        let file = fs::File::open(&path).ok()?;
        let mut content = Vec::new();
        file.take(DESKTOP_ENTRY_MAX_READ_BYTES)
            .read_to_end(&mut content)
            .ok()?;
        for line in content.split(|byte| *byte == b'\n') {
            let Some(value) = line.strip_prefix(b"Name=") else {
                continue;
            };
            let value = String::from_utf8_lossy(value).trim().to_owned();
            if value.is_empty() {
                continue;
            }
            let mut bounded = value;
            bounded.truncate(DESKTOP_ENTRY_NAME_MAX_CHARS);
            return Some(bounded);
        }
        return None;
    }
    None
}

fn add_issue(issues: &mut Vec<IssueCount>, code: &str) {
    if let Some(issue) = issues.iter_mut().find(|issue| issue.code == code) {
        issue.count = issue.count.saturating_add(1);
    } else {
        issues.push(IssueCount::new(code, 1));
    }
}

impl RawSnapshot {
    pub(crate) fn into_protocol(self) -> Result<HelperSnapshot, ProcError> {
        let snapshot = PrivateSnapshot {
            boot_id: self.boot_id,
            euid: self.euid,
            captured_at_unix_ms: self.captured_at_unix_ms,
            captured_at_monotonic_ns: metric_into_protocol(self.captured_at_monotonic_ns),
            total_cpu_jiffies: self.total_cpu_jiffies,
            logical_cpu_count: self.logical_cpu_count,
            processes: self
                .processes
                .into_iter()
                .map(process_into_protocol)
                .collect(),
            cgroups: self.cgroups.into_iter().map(cgroup_into_protocol).collect(),
            applications: self
                .applications
                .into_iter()
                .map(application_into_protocol)
                .collect(),
            system_fd: system_fd_into_protocol(self.system_fd),
            excluded_other_uid: self.excluded_other_uid,
            skipped_race: self.skipped_race,
            permission_denied_counts: self
                .permission_denied_counts
                .into_iter()
                .map(|issue| PrivateIssueCount::new(issue.code, issue.count))
                .collect(),
            issues: self
                .issues
                .into_iter()
                .map(|issue| PrivateIssueCount::new(issue.code, issue.count))
                .collect(),
        };
        snapshot
            .validate()
            .map_err(|error| ProcError::SnapshotInvalid(error.to_string()))?;
        Ok(snapshot)
    }
}

pub(crate) fn raw_from_protocol(snapshot: &PrivateSnapshot) -> Result<RawSnapshot, ProcError> {
    snapshot
        .validate()
        .map_err(|error| ProcError::SnapshotInvalid(error.to_string()))?;
    Ok(RawSnapshot {
        boot_id: snapshot.boot_id.clone(),
        euid: snapshot.euid,
        captured_at_unix_ms: snapshot.captured_at_unix_ms,
        captured_at_monotonic_ns: metric_from_protocol(&snapshot.captured_at_monotonic_ns),
        total_cpu_jiffies: snapshot.total_cpu_jiffies,
        logical_cpu_count: snapshot.logical_cpu_count,
        processes: snapshot
            .processes
            .iter()
            .map(process_from_protocol)
            .collect(),
        cgroups: snapshot.cgroups.iter().map(cgroup_from_protocol).collect(),
        applications: snapshot
            .applications
            .iter()
            .map(application_from_protocol)
            .collect(),
        system_fd: system_fd_from_protocol(&snapshot.system_fd),
        excluded_other_uid: snapshot.excluded_other_uid,
        skipped_race: snapshot.skipped_race,
        permission_denied_counts: snapshot
            .permission_denied_counts
            .iter()
            .map(|issue| IssueCount::new(issue.code.clone(), issue.count))
            .collect(),
        issues: snapshot
            .issues
            .iter()
            .map(|issue| IssueCount::new(issue.code.clone(), issue.count))
            .collect(),
    })
}

fn process_into_protocol(process: RawProcess) -> PrivateProcessRecord {
    PrivateProcessRecord {
        identity: PrivateProcessIdentity {
            boot_id: process.identity.boot_id,
            pid: process.identity.pid,
            start_time_ticks: process.identity.start_time_ticks,
            euid: process.identity.euid,
        },
        ppid: process.ppid,
        comm: process.comm,
        exe_basename: process.exe_basename,
        cgroup_content: process.cgroup_content,
        application_key: process.application_key,
        desktop_entry_id: process.desktop_entry_id,
        grouping_resolution: grouping_into_protocol(process.grouping_resolution),
        cpu_jiffies: process.cpu_jiffies,
        rss_bytes: metric_into_protocol(process.rss_bytes),
        pss_bytes: metric_into_protocol(process.pss_bytes),
        fd_used: metric_into_protocol(process.fd_used),
        fd_soft_limit: metric_into_protocol(process.fd_soft_limit),
        fd_percent_of_soft_limit: metric_into_protocol(process.fd_percent_of_soft_limit),
    }
}

fn process_from_protocol(process: &PrivateProcessRecord) -> RawProcess {
    RawProcess {
        identity: ProcessIdentity {
            boot_id: process.identity.boot_id.clone(),
            pid: process.identity.pid,
            start_time_ticks: process.identity.start_time_ticks,
            euid: process.identity.euid,
        },
        ppid: process.ppid,
        comm: process.comm.clone(),
        exe_basename: process.exe_basename.clone(),
        cgroup_content: process.cgroup_content.clone(),
        application_key: process.application_key.clone(),
        desktop_entry_id: process.desktop_entry_id.clone(),
        grouping_resolution: grouping_from_protocol(process.grouping_resolution),
        cpu_jiffies: process.cpu_jiffies,
        rss_bytes: metric_from_protocol(&process.rss_bytes),
        pss_bytes: metric_from_protocol(&process.pss_bytes),
        fd_used: metric_from_protocol(&process.fd_used),
        fd_soft_limit: metric_from_protocol(&process.fd_soft_limit),
        fd_percent_of_soft_limit: metric_from_protocol(&process.fd_percent_of_soft_limit),
    }
}

fn cgroup_into_protocol(cgroup: RawCgroupRecord) -> PrivateCgroupRecord {
    PrivateCgroupRecord {
        cgroup_path: cgroup.cgroup_path,
        application_key: cgroup.application_key,
        cpu_usage_usec: metric_into_protocol(cgroup.cpu_usage_usec),
        memory_current_bytes: metric_into_protocol(cgroup.memory_current_bytes),
        process_count: metric_into_protocol(cgroup.process_count),
    }
}

fn cgroup_from_protocol(cgroup: &PrivateCgroupRecord) -> RawCgroupRecord {
    RawCgroupRecord {
        cgroup_path: cgroup.cgroup_path.clone(),
        application_key: cgroup.application_key.clone(),
        cpu_usage_usec: metric_from_protocol(&cgroup.cpu_usage_usec),
        memory_current_bytes: metric_from_protocol(&cgroup.memory_current_bytes),
        process_count: metric_from_protocol(&cgroup.process_count),
    }
}

fn system_fd_into_protocol(system_fd: RawSystemFdSnapshot) -> PrivateSystemFdSnapshot {
    PrivateSystemFdSnapshot {
        file_nr_allocated: metric_into_protocol(system_fd.file_nr_allocated),
        file_nr_max: metric_into_protocol(system_fd.file_nr_max),
        file_max: metric_into_protocol(system_fd.file_max),
        pressure_percent: metric_into_protocol(system_fd.pressure_percent),
    }
}

fn application_into_protocol(
    application: RawApplicationResourceRecord,
) -> PrivateApplicationResourceRecord {
    PrivateApplicationResourceRecord {
        application_key: application.application_key,
        process_count: application.process_count,
        proc_cpu_jiffies_sum: metric_into_protocol(application.proc_cpu_jiffies_sum),
        rss_sum_bytes: metric_into_protocol(application.rss_sum_bytes),
        pss_sum_bytes: metric_into_protocol(application.pss_sum_bytes),
        fd_used_sum: metric_into_protocol(application.fd_used_sum),
        fd_soft_limit_sum: metric_into_protocol(application.fd_soft_limit_sum),
        fd_percent_of_attributed_sum: metric_into_protocol(
            application.fd_percent_of_attributed_sum,
        ),
        fd_percent_of_soft_limit_sum: metric_into_protocol(
            application.fd_percent_of_soft_limit_sum,
        ),
        cgroup_cpu_usage_usec: metric_into_protocol(application.cgroup_cpu_usage_usec),
        memory_current_bytes: metric_into_protocol(application.memory_current_bytes),
        cgroup_process_count: metric_into_protocol(application.cgroup_process_count),
    }
}

fn application_from_protocol(
    application: &PrivateApplicationResourceRecord,
) -> RawApplicationResourceRecord {
    RawApplicationResourceRecord {
        application_key: application.application_key.clone(),
        process_count: application.process_count,
        proc_cpu_jiffies_sum: metric_from_protocol(&application.proc_cpu_jiffies_sum),
        rss_sum_bytes: metric_from_protocol(&application.rss_sum_bytes),
        pss_sum_bytes: metric_from_protocol(&application.pss_sum_bytes),
        fd_used_sum: metric_from_protocol(&application.fd_used_sum),
        fd_soft_limit_sum: metric_from_protocol(&application.fd_soft_limit_sum),
        fd_percent_of_attributed_sum: metric_from_protocol(
            &application.fd_percent_of_attributed_sum,
        ),
        fd_percent_of_soft_limit_sum: metric_from_protocol(
            &application.fd_percent_of_soft_limit_sum,
        ),
        cgroup_cpu_usage_usec: metric_from_protocol(&application.cgroup_cpu_usage_usec),
        memory_current_bytes: metric_from_protocol(&application.memory_current_bytes),
        cgroup_process_count: metric_from_protocol(&application.cgroup_process_count),
    }
}

fn system_fd_from_protocol(system_fd: &PrivateSystemFdSnapshot) -> RawSystemFdSnapshot {
    RawSystemFdSnapshot {
        file_nr_allocated: metric_from_protocol(&system_fd.file_nr_allocated),
        file_nr_max: metric_from_protocol(&system_fd.file_nr_max),
        file_max: metric_from_protocol(&system_fd.file_max),
        pressure_percent: metric_from_protocol(&system_fd.pressure_percent),
    }
}

fn metric_into_protocol<T>(metric: MetricValue<T>) -> PrivateMetric<T> {
    if metric.state == MetricState::Known
        && let Some(value) = metric.value
    {
        return PrivateMetric::known(value);
    }
    PrivateMetric::unavailable(
        metric_state_into_protocol(metric.state),
        metric.reason.unwrap_or_else(|| "metric_unknown".to_owned()),
    )
}

fn metric_from_protocol<T: Clone>(metric: &PrivateMetric<T>) -> MetricValue<T> {
    match (&metric.state, &metric.value) {
        (PrivateMetricState::Known, Some(value)) => MetricValue::known(value.clone()),
        _ => MetricValue::unavailable(
            metric_state_from_protocol(metric.state.clone()),
            metric
                .reason
                .clone()
                .unwrap_or_else(|| "metric_unknown".to_owned()),
        ),
    }
}

fn metric_state_into_protocol(state: MetricState) -> PrivateMetricState {
    match state {
        MetricState::Known => PrivateMetricState::Known,
        MetricState::Unknown => PrivateMetricState::Unknown,
        MetricState::PermissionDenied => PrivateMetricState::PermissionDenied,
        MetricState::Raced => PrivateMetricState::Raced,
        MetricState::Unbounded => PrivateMetricState::Unbounded,
        MetricState::WarmingUp => PrivateMetricState::WarmingUp,
        MetricState::SamplingGap => PrivateMetricState::SamplingGap,
    }
}

fn metric_state_from_protocol(state: PrivateMetricState) -> MetricState {
    match state {
        PrivateMetricState::Known => MetricState::Known,
        PrivateMetricState::Unknown => MetricState::Unknown,
        PrivateMetricState::PermissionDenied => MetricState::PermissionDenied,
        PrivateMetricState::Raced => MetricState::Raced,
        PrivateMetricState::Unbounded => MetricState::Unbounded,
        PrivateMetricState::WarmingUp => MetricState::WarmingUp,
        PrivateMetricState::SamplingGap => MetricState::SamplingGap,
    }
}

fn grouping_into_protocol(grouping: GroupingResolution) -> PrivateGroupingResolution {
    match grouping {
        GroupingResolution::DesktopEntryExact => PrivateGroupingResolution::DesktopEntryExact,
        GroupingResolution::CgroupScope => PrivateGroupingResolution::CgroupScope,
        GroupingResolution::InheritedParent => PrivateGroupingResolution::InheritedParent,
        GroupingResolution::Unknown => PrivateGroupingResolution::Unknown,
    }
}

fn grouping_from_protocol(grouping: PrivateGroupingResolution) -> GroupingResolution {
    match grouping {
        PrivateGroupingResolution::DesktopEntryExact => GroupingResolution::DesktopEntryExact,
        PrivateGroupingResolution::CgroupScope => GroupingResolution::CgroupScope,
        PrivateGroupingResolution::InheritedParent => GroupingResolution::InheritedParent,
        PrivateGroupingResolution::Unknown => GroupingResolution::Unknown,
    }
}

#[cfg(test)]
mod desktop_entry_tests {
    use super::{desktop_display_name, desktop_id_for_scope, desktop_name_candidates};
    use std::{fs, path::PathBuf};
    use tempfile::tempdir;

    #[test]
    fn exact_scope_label_matches_desktop_file() {
        let root = tempdir().expect("root");
        fs::write(
            root.path().join("org.example.App.desktop"),
            b"[Desktop Entry]\n",
        )
        .unwrap();
        let scope = "0::/user.slice/app-org.example.App.scope";
        assert_eq!(
            desktop_id_for_scope(scope, &[root.path().to_owned()]),
            Some("org.example.App.desktop".to_owned())
        );
    }

    #[test]
    fn pid_suffixed_scope_label_strips_the_pid_suffix() {
        let root = tempdir().expect("root");
        fs::write(
            root.path().join("codex-desktop.desktop"),
            b"[Desktop Entry]\n",
        )
        .unwrap();
        let scope = "0::/user.slice/app-codex-desktop-970898.scope";
        assert_eq!(
            desktop_id_for_scope(scope, &[root.path().to_owned()]),
            Some("codex-desktop.desktop".to_owned())
        );
        // A numeric-looking base must not be over-stripped when no file matches.
        let scope = "0::/user.slice/app-org.example.Worker-4242.scope";
        assert_eq!(desktop_id_for_scope(scope, &[root.path().to_owned()]), None);
    }

    #[test]
    fn template_instance_scope_strips_the_instance() {
        let root = tempdir().expect("root");
        fs::write(
            root.path().join("org.example.Foo.desktop"),
            b"[Desktop Entry]\n",
        )
        .unwrap();
        let scope = "0::/user.slice/app-org.example.Foo@instance-4242.scope";
        assert_eq!(
            desktop_id_for_scope(scope, &[root.path().to_owned()]),
            Some("org.example.Foo.desktop".to_owned())
        );
    }

    #[test]
    fn transient_run_scope_never_matches() {
        let root = tempdir().expect("root");
        fs::write(
            root.path().join("codex-desktop.desktop"),
            b"[Desktop Entry]\n",
        )
        .unwrap();
        let scope = "0::/user.slice/run-p967465-i968317.scope";
        assert_eq!(desktop_id_for_scope(scope, &[root.path().to_owned()]), None);
    }

    #[test]
    fn candidates_are_most_specific_first() {
        assert_eq!(
            desktop_name_candidates("codex-desktop-970898"),
            vec![
                "codex-desktop-970898".to_owned(),
                "codex-desktop".to_owned()
            ]
        );
        assert_eq!(
            desktop_name_candidates("org.example.Foo@instance-4242"),
            vec![
                "org.example.Foo@instance-4242".to_owned(),
                "org.example.Foo@instance".to_owned(),
                "org.example.Foo".to_owned()
            ]
        );
    }

    #[test]
    fn display_name_reads_the_bounded_name_field() {
        let root = tempdir().expect("root");
        fs::write(
            root.path().join("codex-desktop.desktop"),
            b"[Desktop Entry]\nName=ChatGPT\nName[zh_CN]=ChatGPT\nIcon=codex-desktop\n",
        )
        .unwrap();
        assert_eq!(
            desktop_display_name(
                "codex-desktop.desktop",
                &[PathBuf::from("/nonexistent"), root.path().to_owned()]
            ),
            Some("ChatGPT".to_owned())
        );
        assert_eq!(
            desktop_display_name("missing.desktop", &[root.path().to_owned()]),
            None
        );
        assert_eq!(
            desktop_display_name("missing.desktop", &[PathBuf::from("/nonexistent")]),
            None
        );
    }
}
