#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    io::{self, Read, Write},
};
use thiserror::Error;

pub const PROTOCOL_VERSION: u16 = 3;
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub const MAX_PROCESS_RECORDS: usize = 4_096;
pub const MAX_APPLICATION_RECORDS: usize = 1_024;
pub const MAX_CGROUP_RECORDS: usize = MAX_PROCESS_RECORDS;
pub const MAX_ISSUE_RECORDS: usize = 256;
pub const MAX_STRING_BYTES: usize = 4_096;
pub const MAX_REASON_BYTES: usize = 256;

pub type HelperRequest = CollectionRequest;
pub type HelperReply = CollectionReply;
pub type HelperReplyBody = CollectionReplyBody;
pub type HelperSnapshot = PrivateSnapshot;

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("frame read failed: {0}")]
    Io(#[source] io::Error),
    #[error("frame ended before its length prefix completed")]
    TruncatedLength,
    #[error("frame ended before its payload completed")]
    TruncatedPayload,
    #[error("frame is empty")]
    Empty,
    #[error("frame length {length} exceeds maximum {max}")]
    Oversized { length: usize, max: usize },
    #[error("frame length does not fit in a 32-bit prefix")]
    LengthOverflow,
    #[error("JSON payload is malformed: {0}")]
    MalformedJson(#[source] serde_json::Error),
    #[error("unsupported protocol version: {0}")]
    UnsupportedVersion(u16),
    #[error("protocol payload is invalid: {0}")]
    InvalidPayload(String),
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    Collect,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CollectionRequest {
    pub version: u16,
    pub generation: u64,
    pub kind: RequestKind,
}

impl CollectionRequest {
    pub fn collect(generation: u64) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            generation,
            kind: RequestKind::Collect,
        }
    }

    pub fn validate(&self) -> Result<(), FrameError> {
        validate_version(self.version)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HelperErrorCode {
    MalformedRequest,
    UnsupportedVersion,
    OversizedFrame,
    InvalidRequest,
    ProcUnavailable,
    ProcPermissionDenied,
    ProcRaced,
    ProcInvalid,
    LimitExceeded,
    ShuttingDown,
    Internal,
}

impl HelperErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MalformedRequest => "malformed_request",
            Self::UnsupportedVersion => "unsupported_version",
            Self::OversizedFrame => "oversized_frame",
            Self::InvalidRequest => "invalid_request",
            Self::ProcUnavailable => "proc_unavailable",
            Self::ProcPermissionDenied => "proc_permission_denied",
            Self::ProcRaced => "proc_raced",
            Self::ProcInvalid => "proc_invalid",
            Self::LimitExceeded => "limit_exceeded",
            Self::ShuttingDown => "shutting_down",
            Self::Internal => "internal",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HelperError {
    pub code: HelperErrorCode,
    pub retryable: bool,
    pub reason: String,
}

impl HelperError {
    pub fn new(code: HelperErrorCode, retryable: bool, reason: impl Into<String>) -> Self {
        Self {
            code,
            retryable,
            reason: reason.into(),
        }
    }

    fn validate(&self) -> Result<(), FrameError> {
        if self.reason.is_empty() || self.reason.len() > MAX_REASON_BYTES {
            return Err(FrameError::InvalidPayload(
                "helper error reason is empty or oversized".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivateMetricState {
    Known,
    Unknown,
    PermissionDenied,
    Raced,
    Unbounded,
    WarmingUp,
    SamplingGap,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateMetric<T> {
    pub value: Option<T>,
    pub state: PrivateMetricState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl<T> PrivateMetric<T> {
    pub fn known(value: T) -> Self {
        Self {
            value: Some(value),
            state: PrivateMetricState::Known,
            reason: None,
        }
    }

    pub fn unavailable(state: PrivateMetricState, reason: impl Into<String>) -> Self {
        Self {
            value: None,
            state,
            reason: Some(reason.into()),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateProcessIdentity {
    pub boot_id: String,
    pub pid: u32,
    pub start_time_ticks: u64,
    pub euid: u32,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivateGroupingResolution {
    DesktopEntryExact,
    CgroupScope,
    InheritedParent,
    Unknown,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateIssueCount {
    pub code: String,
    pub count: u64,
}

impl PrivateIssueCount {
    pub fn new(code: impl Into<String>, count: u64) -> Self {
        Self {
            code: code.into(),
            count,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateProcessRecord {
    pub identity: PrivateProcessIdentity,
    pub ppid: u32,
    pub comm: String,
    pub exe_basename: Option<String>,
    pub cgroup_content: String,
    pub application_key: String,
    pub desktop_entry_id: Option<String>,
    pub grouping_resolution: PrivateGroupingResolution,
    pub cpu_jiffies: u64,
    pub rss_bytes: PrivateMetric<u64>,
    pub pss_bytes: PrivateMetric<u64>,
    pub fd_used: PrivateMetric<u64>,
    pub fd_soft_limit: PrivateMetric<u64>,
    pub fd_percent_of_soft_limit: PrivateMetric<f64>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateCgroupRecord {
    pub cgroup_path: String,
    pub application_key: String,
    pub cpu_usage_usec: PrivateMetric<u64>,
    pub memory_current_bytes: PrivateMetric<u64>,
    pub process_count: PrivateMetric<u64>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateApplicationResourceRecord {
    pub application_key: String,
    pub process_count: u64,
    pub proc_cpu_jiffies_sum: PrivateMetric<u64>,
    pub rss_sum_bytes: PrivateMetric<u64>,
    pub pss_sum_bytes: PrivateMetric<u64>,
    pub fd_used_sum: PrivateMetric<u64>,
    pub fd_soft_limit_sum: PrivateMetric<u64>,
    pub fd_percent_of_attributed_sum: PrivateMetric<f64>,
    pub fd_percent_of_soft_limit_sum: PrivateMetric<f64>,
    pub cgroup_cpu_usage_usec: PrivateMetric<u64>,
    pub memory_current_bytes: PrivateMetric<u64>,
    pub cgroup_process_count: PrivateMetric<u64>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateSystemFdSnapshot {
    pub file_nr_allocated: PrivateMetric<u64>,
    pub file_nr_max: PrivateMetric<u64>,
    pub file_max: PrivateMetric<u64>,
    pub pressure_percent: PrivateMetric<f64>,
}

impl PrivateSystemFdSnapshot {
    pub fn unavailable(state: PrivateMetricState, reason: &str) -> Self {
        Self {
            file_nr_allocated: PrivateMetric::unavailable(state.clone(), reason),
            file_nr_max: PrivateMetric::unavailable(state.clone(), reason),
            file_max: PrivateMetric::unavailable(state.clone(), reason),
            pressure_percent: PrivateMetric::unavailable(state, reason),
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateSnapshot {
    pub boot_id: String,
    pub euid: u32,
    pub captured_at_unix_ms: i64,
    pub captured_at_monotonic_ns: PrivateMetric<u64>,
    pub total_cpu_jiffies: u64,
    pub logical_cpu_count: u32,
    pub processes: Vec<PrivateProcessRecord>,
    pub cgroups: Vec<PrivateCgroupRecord>,
    pub applications: Vec<PrivateApplicationResourceRecord>,
    pub system_fd: PrivateSystemFdSnapshot,
    pub excluded_other_uid: u64,
    pub skipped_race: u64,
    pub permission_denied_counts: Vec<PrivateIssueCount>,
    pub issues: Vec<PrivateIssueCount>,
}

impl PrivateSnapshot {
    pub fn validate(&self) -> Result<(), FrameError> {
        validate_string(&self.boot_id, "boot_id")?;
        validate_metric(&self.captured_at_monotonic_ns)?;
        if self.logical_cpu_count == 0 {
            return Err(FrameError::InvalidPayload(
                "logical_cpu_count must be positive".to_owned(),
            ));
        }
        if self.processes.len() > MAX_PROCESS_RECORDS {
            return Err(FrameError::InvalidPayload(format!(
                "process record count exceeds {MAX_PROCESS_RECORDS}"
            )));
        }
        if self.cgroups.len() > MAX_CGROUP_RECORDS {
            return Err(FrameError::InvalidPayload(format!(
                "cgroup record count exceeds {MAX_CGROUP_RECORDS}"
            )));
        }
        if self.applications.len() > MAX_APPLICATION_RECORDS {
            return Err(FrameError::InvalidPayload(format!(
                "application record count exceeds {MAX_APPLICATION_RECORDS}"
            )));
        }
        validate_issue_counts(&self.permission_denied_counts)?;
        validate_issue_counts(&self.issues)?;
        for process in &self.processes {
            validate_string(&process.identity.boot_id, "process.boot_id")?;
            if process.identity.boot_id != self.boot_id || process.identity.euid != self.euid {
                return Err(FrameError::InvalidPayload(
                    "process identity does not match snapshot scope".to_owned(),
                ));
            }
            validate_string(&process.comm, "process.comm")?;
            validate_string(&process.cgroup_content, "process.cgroup_content")?;
            validate_string(&process.application_key, "process.application_key")?;
            if let Some(exe) = &process.exe_basename {
                validate_string(exe, "process.exe_basename")?;
            }
            if let Some(desktop) = &process.desktop_entry_id {
                validate_string(desktop, "process.desktop_entry_id")?;
            }
            validate_metric(&process.rss_bytes)?;
            validate_metric(&process.pss_bytes)?;
            validate_metric(&process.fd_used)?;
            validate_metric(&process.fd_soft_limit)?;
            validate_metric(&process.fd_percent_of_soft_limit)?;
        }
        for cgroup in &self.cgroups {
            validate_string(&cgroup.cgroup_path, "cgroup.cgroup_path")?;
            if !cgroup.cgroup_path.starts_with('/') {
                return Err(FrameError::InvalidPayload(
                    "cgroup.cgroup_path must be absolute".to_owned(),
                ));
            }
            validate_string(&cgroup.application_key, "cgroup.application_key")?;
            validate_metric(&cgroup.cpu_usage_usec)?;
            validate_metric(&cgroup.memory_current_bytes)?;
            validate_metric(&cgroup.process_count)?;
        }
        for application in &self.applications {
            validate_string(&application.application_key, "application.application_key")?;
            validate_metric(&application.proc_cpu_jiffies_sum)?;
            validate_metric(&application.rss_sum_bytes)?;
            validate_metric(&application.pss_sum_bytes)?;
            validate_metric(&application.fd_used_sum)?;
            validate_metric(&application.fd_soft_limit_sum)?;
            validate_metric(&application.fd_percent_of_attributed_sum)?;
            validate_metric(&application.fd_percent_of_soft_limit_sum)?;
            validate_metric(&application.cgroup_cpu_usage_usec)?;
            validate_metric(&application.memory_current_bytes)?;
            validate_metric(&application.cgroup_process_count)?;
        }
        validate_metric(&self.system_fd.file_nr_allocated)?;
        validate_metric(&self.system_fd.file_nr_max)?;
        validate_metric(&self.system_fd.file_max)?;
        validate_metric(&self.system_fd.pressure_percent)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub enum CollectionReplyBody {
    Snapshot(Box<PrivateSnapshot>),
    Error(HelperError),
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CollectionReply {
    pub version: u16,
    pub generation: u64,
    pub body: CollectionReplyBody,
}

impl CollectionReply {
    pub fn snapshot(generation: u64, snapshot: PrivateSnapshot) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            generation,
            body: CollectionReplyBody::Snapshot(Box::new(snapshot)),
        }
    }

    pub fn error(generation: u64, error: HelperError) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            generation,
            body: CollectionReplyBody::Error(error),
        }
    }

    pub fn validate(&self) -> Result<(), FrameError> {
        validate_version(self.version)?;
        match &self.body {
            CollectionReplyBody::Snapshot(snapshot) => snapshot.validate(),
            CollectionReplyBody::Error(error) => error.validate(),
        }
    }
}

pub fn read_frame<R: Read>(reader: &mut R) -> Result<Option<Vec<u8>>, FrameError> {
    let mut first = [0_u8; 1];
    match reader.read_exact(&mut first) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(FrameError::Io(error)),
    }
    let mut rest = [0_u8; 3];
    reader
        .read_exact(&mut rest)
        .map_err(|_| FrameError::TruncatedLength)?;
    let length = u32::from_be_bytes([first[0], rest[0], rest[1], rest[2]]) as usize;
    if length == 0 {
        return Err(FrameError::Empty);
    }
    if length > MAX_FRAME_BYTES {
        return Err(FrameError::Oversized {
            length,
            max: MAX_FRAME_BYTES,
        });
    }
    let mut payload = vec![0_u8; length];
    reader
        .read_exact(&mut payload)
        .map_err(|_| FrameError::TruncatedPayload)?;
    Ok(Some(payload))
}

pub fn write_frame<W: Write>(writer: &mut W, payload: &[u8]) -> Result<(), FrameError> {
    if payload.is_empty() {
        return Err(FrameError::Empty);
    }
    if payload.len() > MAX_FRAME_BYTES {
        return Err(FrameError::Oversized {
            length: payload.len(),
            max: MAX_FRAME_BYTES,
        });
    }
    let length = u32::try_from(payload.len()).map_err(|_| FrameError::LengthOverflow)?;
    writer
        .write_all(&length.to_be_bytes())
        .and_then(|()| writer.write_all(payload))
        .and_then(|()| writer.flush())
        .map_err(FrameError::Io)
}

pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, FrameError> {
    let payload = serde_json::to_vec(value).map_err(FrameError::MalformedJson)?;
    if payload.is_empty() {
        return Err(FrameError::Empty);
    }
    if payload.len() > MAX_FRAME_BYTES {
        return Err(FrameError::Oversized {
            length: payload.len(),
            max: MAX_FRAME_BYTES,
        });
    }
    Ok(payload)
}

pub fn decode_request(payload: &[u8]) -> Result<CollectionRequest, FrameError> {
    ensure_payload_size(payload)?;
    let request: CollectionRequest =
        serde_json::from_slice(payload).map_err(FrameError::MalformedJson)?;
    request.validate()?;
    Ok(request)
}

pub fn decode_reply(payload: &[u8]) -> Result<CollectionReply, FrameError> {
    ensure_payload_size(payload)?;
    let reply: CollectionReply =
        serde_json::from_slice(payload).map_err(FrameError::MalformedJson)?;
    reply.validate()?;
    Ok(reply)
}

pub fn read_request<R: Read>(reader: &mut R) -> Result<Option<CollectionRequest>, FrameError> {
    read_frame(reader)?
        .map(|payload| decode_request(&payload))
        .transpose()
}

pub fn write_request<W: Write>(
    writer: &mut W,
    request: &CollectionRequest,
) -> Result<(), FrameError> {
    request.validate()?;
    let payload = encode(request)?;
    write_frame(writer, &payload)
}

pub fn read_reply<R: Read>(reader: &mut R) -> Result<Option<CollectionReply>, FrameError> {
    read_frame(reader)?
        .map(|payload| decode_reply(&payload))
        .transpose()
}

pub fn write_reply<W: Write>(writer: &mut W, reply: &CollectionReply) -> Result<(), FrameError> {
    let reply = bounded_reply(reply)?;
    let payload = encode(&reply)?;
    write_frame(writer, &payload)
}

fn bounded_reply(reply: &CollectionReply) -> Result<CollectionReply, FrameError> {
    reply.validate()?;
    let payload = serde_json::to_vec(reply).map_err(FrameError::MalformedJson)?;
    if payload.len() <= MAX_FRAME_BYTES {
        return Ok(reply.clone());
    }
    let CollectionReplyBody::Snapshot(_) = &reply.body else {
        return Err(FrameError::Oversized {
            length: payload.len(),
            max: MAX_FRAME_BYTES,
        });
    };

    let mut metadata_bounded = reply.clone();
    let CollectionReplyBody::Snapshot(snapshot) = &mut metadata_bounded.body else {
        unreachable!("snapshot body checked above");
    };
    snapshot
        .permission_denied_counts
        .sort_by(|left, right| left.code.cmp(&right.code));
    snapshot
        .issues
        .sort_by(|left, right| left.code.cmp(&right.code));
    add_or_replace_issue(&mut snapshot.issues, "reply_budget_exceeded", 1);
    match trim_issue_metadata_to_fit(&mut metadata_bounded) {
        Ok(()) => {
            metadata_bounded.validate()?;
            return Ok(metadata_bounded);
        }
        Err(FrameError::Oversized { .. }) => {}
        Err(error) => return Err(error),
    }
    let CollectionReplyBody::Snapshot(snapshot) = &metadata_bounded.body else {
        unreachable!("snapshot body checked above");
    };

    let application_keys = snapshot
        .applications
        .iter()
        .map(|application| application.application_key.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let mut lower = 0_usize;
    let mut upper = application_keys.len();
    while lower < upper {
        let candidate_count = lower + (upper - lower).div_ceil(2);
        let candidate = bounded_snapshot_reply(
            reply.generation,
            snapshot,
            &application_keys[..candidate_count],
        );
        if serialized_len(&candidate)? <= MAX_FRAME_BYTES {
            lower = candidate_count;
        } else {
            upper = candidate_count - 1;
        }
    }

    let mut bounded =
        bounded_snapshot_reply(reply.generation, snapshot, &application_keys[..lower]);
    trim_issue_metadata_to_fit(&mut bounded)?;
    bounded.validate()?;
    Ok(bounded)
}

fn bounded_snapshot_reply(
    generation: u64,
    snapshot: &PrivateSnapshot,
    retained_keys: &[String],
) -> CollectionReply {
    let retained = retained_keys.iter().cloned().collect::<BTreeSet<_>>();
    let mut bounded = snapshot.clone();
    bounded
        .processes
        .retain(|process| retained.contains(&process.application_key));
    bounded
        .cgroups
        .retain(|cgroup| retained.contains(&cgroup.application_key));
    bounded
        .applications
        .retain(|application| retained.contains(&application.application_key));

    let dropped_records = snapshot
        .processes
        .len()
        .saturating_sub(bounded.processes.len())
        .saturating_add(snapshot.cgroups.len().saturating_sub(bounded.cgroups.len()))
        .saturating_add(
            snapshot
                .applications
                .len()
                .saturating_sub(bounded.applications.len()),
        );
    if dropped_records > 0 {
        for application in &mut bounded.applications {
            application.fd_percent_of_attributed_sum =
                PrivateMetric::unavailable(PrivateMetricState::Unknown, "reply_budget_reduced");
        }
    }
    add_or_replace_issue(
        &mut bounded.issues,
        "reply_budget_exceeded",
        u64::try_from(dropped_records).unwrap_or(u64::MAX).max(1),
    );
    CollectionReply::snapshot(generation, bounded)
}

fn add_or_replace_issue(issues: &mut Vec<PrivateIssueCount>, code: &str, count: u64) {
    if let Some(issue) = issues.iter_mut().find(|issue| issue.code == code) {
        issue.count = issue.count.saturating_add(count);
        return;
    }
    if issues.len() == MAX_ISSUE_RECORDS {
        issues.pop();
    }
    issues.push(PrivateIssueCount::new(code, count));
}

fn trim_issue_metadata_to_fit(reply: &mut CollectionReply) -> Result<(), FrameError> {
    loop {
        let length = serialized_len(reply)?;
        if length <= MAX_FRAME_BYTES {
            return Ok(());
        }
        let CollectionReplyBody::Snapshot(snapshot) = &mut reply.body else {
            return Err(FrameError::Oversized {
                length,
                max: MAX_FRAME_BYTES,
            });
        };
        if snapshot.permission_denied_counts.pop().is_some() {
            continue;
        }
        if let Some(index) = snapshot
            .issues
            .iter()
            .rposition(|issue| issue.code != "reply_budget_exceeded")
        {
            snapshot.issues.remove(index);
            continue;
        }
        return Err(FrameError::Oversized {
            length,
            max: MAX_FRAME_BYTES,
        });
    }
}

fn serialized_len<T: Serialize>(value: &T) -> Result<usize, FrameError> {
    serde_json::to_vec(value)
        .map(|payload| payload.len())
        .map_err(FrameError::MalformedJson)
}

fn validate_version(version: u16) -> Result<(), FrameError> {
    if version == PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(FrameError::UnsupportedVersion(version))
    }
}

fn ensure_payload_size(payload: &[u8]) -> Result<(), FrameError> {
    if payload.is_empty() {
        return Err(FrameError::Empty);
    }
    if payload.len() > MAX_FRAME_BYTES {
        return Err(FrameError::Oversized {
            length: payload.len(),
            max: MAX_FRAME_BYTES,
        });
    }
    Ok(())
}

fn validate_string(value: &str, field: &str) -> Result<(), FrameError> {
    if value.is_empty() || value.len() > MAX_STRING_BYTES || value.contains('\0') {
        return Err(FrameError::InvalidPayload(format!(
            "{field} is empty, contains NUL, or is oversized"
        )));
    }
    Ok(())
}

fn validate_issue_counts(issues: &[PrivateIssueCount]) -> Result<(), FrameError> {
    if issues.len() > MAX_ISSUE_RECORDS {
        return Err(FrameError::InvalidPayload(format!(
            "issue record count exceeds {MAX_ISSUE_RECORDS}"
        )));
    }
    for issue in issues {
        validate_string(&issue.code, "issue.code")?;
    }
    Ok(())
}

fn validate_metric<T: Serialize>(metric: &PrivateMetric<T>) -> Result<(), FrameError> {
    if let Some(reason) = &metric.reason
        && (reason.is_empty() || reason.len() > MAX_REASON_BYTES || reason.contains('\0'))
    {
        return Err(FrameError::InvalidPayload(
            "metric reason is empty, contains NUL, or is oversized".to_owned(),
        ));
    }
    if metric.state == PrivateMetricState::Known && metric.value.is_none() {
        return Err(FrameError::InvalidPayload(
            "known metric is missing its value".to_owned(),
        ));
    }
    if metric.state != PrivateMetricState::Known && metric.value.is_some() {
        return Err(FrameError::InvalidPayload(
            "unavailable metric must not carry a value".to_owned(),
        ));
    }
    Ok(())
}
