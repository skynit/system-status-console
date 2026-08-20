use nix::unistd::Uid;
use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    fs::{self, File},
    io::{BufRead, BufReader},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const CORPUS_SCHEMA_VERSION: u16 = 1;
const MIN_DAY_WINDOW_MS: i64 = 20 * 60 * 60 * 1_000;
const MAX_DAY_WINDOW_MS: i64 = 28 * 60 * 60 * 1_000;
const MAX_SOURCE_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SESSION_TEXT_CHARS: usize = 64 * 1024;
const MAX_MESSAGE_TEXT_CHARS: usize = 16 * 1024;
const MAX_TOTAL_TEXT_CHARS: usize = 1024 * 1024;
const MAX_SESSIONS: usize = 64;
const SHORT_SESSION_MESSAGES: u32 = 4;
const SHORT_SESSION_CHARS: usize = 1_200;
const LONG_SESSION_MESSAGES: u32 = 24;
const LONG_SESSION_CHARS: usize = 12_000;
const TOKEN_SYNC_STALE_MS: i64 = 5 * 60 * 1_000;

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FactState {
    Healthy,
    Degraded,
    Unsupported,
    Unreachable,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceCoverage {
    pub source: String,
    pub state: FactState,
    pub reason: String,
    pub scanned_sessions: Option<u32>,
    pub included_sessions: Option<u32>,
    pub ignored_short_sessions: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct TokenSourceUsage {
    pub source: String,
    pub request_count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub reported_total_tokens: u64,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct TokenUsage {
    pub state: FactState,
    pub reason: String,
    pub window_start_ms: i64,
    pub window_end_ms: i64,
    pub last_synced_at_ms: Option<i64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_creation_tokens: Option<u64>,
    pub reported_total_tokens: Option<u64>,
    pub total_method: String,
    pub by_source: Vec<TokenSourceUsage>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EligibilityState {
    Included,
    IgnoredShort,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct SessionEligibility {
    pub state: EligibilityState,
    pub reason: String,
    pub substantive_messages: u32,
    pub content_chars: u32,
    pub length_class: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct NormalizedMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct NormalizedSession {
    pub source: String,
    pub session_id: String,
    pub title: String,
    pub workspace: Option<String>,
    pub updated_at_ms: i64,
    pub eligibility: SessionEligibility,
    pub messages: Vec<NormalizedMessage>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct JournalCorpus {
    pub schema_version: u16,
    pub local_date: String,
    pub timezone: String,
    pub source_coverage: Vec<SourceCoverage>,
    pub token_usage: TokenUsage,
    pub sessions: Vec<NormalizedSession>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalFetchRequest {
    pub local_date: String,
    pub timezone: String,
    pub window_start_ms: i64,
    pub window_end_ms: i64,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct JournalFetchError {
    pub code: String,
    pub reason: String,
    pub retryable: bool,
}

impl JournalFetchError {
    fn new(code: impl Into<String>, retryable: bool) -> Self {
        let code = code.into();
        Self {
            reason: code.clone(),
            code,
            retryable,
        }
    }
}

#[derive(Debug, Clone)]
struct CollectorPaths {
    #[cfg(test)]
    home: PathBuf,
    cc_switch_db: PathBuf,
    codex_sessions: PathBuf,
    claude_projects: PathBuf,
    opencode_db: PathBuf,
}

impl CollectorPaths {
    fn from_environment() -> Result<Self, JournalFetchError> {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| JournalFetchError::new("home_directory_unavailable", false))?;
        Ok(Self {
            cc_switch_db: home.join(".cc-switch/cc-switch.db"),
            codex_sessions: home.join(".codex/sessions"),
            claude_projects: home.join(".claude/projects"),
            opencode_db: home.join(".local/share/opencode/opencode.db"),
            #[cfg(test)]
            home,
        })
    }
}

#[derive(Debug, Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
enum SessionSource {
    Claude,
    Codex,
    OpenCode,
}

impl SessionSource {
    const ALL: [Self; 3] = [Self::Codex, Self::Claude, Self::OpenCode];

    const fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
        }
    }
}

#[derive(Debug)]
struct TrackedPath {
    source: SessionSource,
    path: PathBuf,
    updated_at_ms: i64,
}

#[derive(Debug, Default)]
struct SourceResult {
    sessions: Vec<NormalizedSession>,
    errors: Vec<String>,
    tracked: bool,
}

pub(crate) struct JournalCollector {
    paths: CollectorPaths,
}

impl JournalCollector {
    pub(crate) fn from_environment() -> Result<Self, JournalFetchError> {
        Ok(Self {
            paths: CollectorPaths::from_environment()?,
        })
    }

    #[cfg(test)]
    fn new(paths: CollectorPaths) -> Self {
        Self { paths }
    }

    pub(crate) fn collect(
        &self,
        request: &JournalFetchRequest,
    ) -> Result<JournalCorpus, JournalFetchError> {
        validate_request(request)?;
        validate_owned_regular_file(&self.paths.cc_switch_db, "cc_switch_database_unavailable")?;
        let database = open_read_only(&self.paths.cc_switch_db)
            .map_err(|_| JournalFetchError::new("cc_switch_database_unavailable", true))?;

        let token_usage = collect_token_usage(&database, request)?;
        let configured = configured_sources(&database).unwrap_or_default();
        let tracked = tracked_paths(&database, &self.paths, request)?;
        let mut results: BTreeMap<SessionSource, SourceResult> = SessionSource::ALL
            .into_iter()
            .map(|source| (source, SourceResult::default()))
            .collect();

        for entry in tracked {
            let result = results.get_mut(&entry.source).expect("known source");
            result.tracked = true;
            if result.sessions.len() >= MAX_SESSIONS {
                result
                    .errors
                    .push("session_candidate_limit_reached".to_owned());
                continue;
            }
            match entry.source {
                SessionSource::Codex => match parse_codex_session(
                    &entry.path,
                    entry.updated_at_ms,
                    &self.paths.codex_sessions,
                ) {
                    Ok(session) => result.sessions.push(session),
                    Err(reason) => result.errors.push(reason),
                },
                SessionSource::Claude => match parse_claude_session(
                    &entry.path,
                    entry.updated_at_ms,
                    &self.paths.claude_projects,
                ) {
                    Ok(session) => result.sessions.push(session),
                    Err(reason) => result.errors.push(reason),
                },
                SessionSource::OpenCode => {}
            }
        }

        if results
            .get(&SessionSource::OpenCode)
            .is_some_and(|result| result.tracked)
        {
            match collect_opencode_sessions(&self.paths.opencode_db, request) {
                Ok(sessions) => {
                    results
                        .get_mut(&SessionSource::OpenCode)
                        .expect("known source")
                        .sessions = sessions
                }
                Err(reason) => results
                    .get_mut(&SessionSource::OpenCode)
                    .expect("known source")
                    .errors
                    .push(reason),
            }
        }

        let mut source_coverage = Vec::new();
        let mut sessions = Vec::new();
        let mut warnings = Vec::new();
        for source in SessionSource::ALL {
            let result = results.remove(&source).expect("known source");
            let configured_or_present = configured.contains(source.name()) || result.tracked;
            if !configured_or_present {
                source_coverage.push(SourceCoverage {
                    source: source.name().to_owned(),
                    state: FactState::Unsupported,
                    reason: "cc_switch_session_source_not_configured".to_owned(),
                    scanned_sessions: None,
                    included_sessions: None,
                    ignored_short_sessions: None,
                });
                continue;
            }
            if !result.tracked {
                source_coverage.push(SourceCoverage {
                    source: source.name().to_owned(),
                    state: FactState::Degraded,
                    reason: "cc_switch_session_source_not_tracked".to_owned(),
                    scanned_sessions: Some(0),
                    included_sessions: Some(0),
                    ignored_short_sessions: Some(0),
                });
                continue;
            }

            let scanned = result.sessions.len() as u32;
            let included = result
                .sessions
                .iter()
                .filter(|session| session.eligibility.state == EligibilityState::Included)
                .count() as u32;
            let ignored = scanned.saturating_sub(included);
            let state = if result.errors.is_empty() {
                FactState::Healthy
            } else {
                warnings.extend(
                    result
                        .errors
                        .iter()
                        .map(|reason| format!("{}_{}", source.name(), reason)),
                );
                FactState::Degraded
            };
            source_coverage.push(SourceCoverage {
                source: source.name().to_owned(),
                reason: if state == FactState::Healthy {
                    "session_source_ready".to_owned()
                } else {
                    "session_source_partial".to_owned()
                },
                state,
                scanned_sessions: Some(scanned),
                included_sessions: Some(included),
                ignored_short_sessions: Some(ignored),
            });
            sessions.extend(result.sessions);
        }

        sessions.sort_by(|left, right| {
            right
                .updated_at_ms
                .cmp(&left.updated_at_ms)
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        sessions.truncate(MAX_SESSIONS);
        enforce_corpus_text_limit(&mut sessions, &mut warnings);

        Ok(JournalCorpus {
            schema_version: CORPUS_SCHEMA_VERSION,
            local_date: request.local_date.clone(),
            timezone: request.timezone.clone(),
            source_coverage,
            token_usage,
            sessions,
            warnings,
        })
    }
}

fn validate_request(request: &JournalFetchRequest) -> Result<(), JournalFetchError> {
    let bytes = request.local_date.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes
            .iter()
            .enumerate()
            .any(|(index, byte)| index != 4 && index != 7 && !byte.is_ascii_digit())
    {
        return Err(JournalFetchError::new("journal_local_date_invalid", false));
    }
    if request.timezone.is_empty()
        || request.timezone.len() > 64
        || request.timezone.chars().any(char::is_control)
    {
        return Err(JournalFetchError::new("journal_timezone_invalid", false));
    }
    let duration = request
        .window_end_ms
        .checked_sub(request.window_start_ms)
        .ok_or_else(|| JournalFetchError::new("journal_time_window_invalid", false))?;
    if request.window_start_ms < 0 || duration < MIN_DAY_WINDOW_MS || duration > MAX_DAY_WINDOW_MS {
        return Err(JournalFetchError::new("journal_time_window_invalid", false));
    }
    Ok(())
}

fn open_read_only(path: &Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
}

fn validate_owned_regular_file(path: &Path, reason: &str) -> Result<(), JournalFetchError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| JournalFetchError::new(reason, true))?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != Uid::effective().as_raw()
    {
        return Err(JournalFetchError::new(reason, false));
    }
    Ok(())
}

fn collect_token_usage(
    database: &Connection,
    request: &JournalFetchRequest,
) -> Result<TokenUsage, JournalFetchError> {
    let start_seconds = request.window_start_ms.div_euclid(1_000);
    let end_seconds = request.window_end_ms.div_euclid(1_000);
    let mut statement = database
        .prepare(
            "SELECT app_type, COUNT(*), COALESCE(SUM(input_tokens), 0), \
             COALESCE(SUM(output_tokens), 0), COALESCE(SUM(cache_read_tokens), 0), \
             COALESCE(SUM(cache_creation_tokens), 0) \
             FROM proxy_request_logs WHERE created_at >= ?1 AND created_at < ?2 \
             GROUP BY app_type ORDER BY app_type",
        )
        .map_err(|_| JournalFetchError::new("cc_switch_usage_query_failed", true))?;
    let rows = statement
        .query_map(params![start_seconds, end_seconds], |row| {
            let input_tokens = nonnegative_u64(row.get::<_, i64>(2)?)?;
            let output_tokens = nonnegative_u64(row.get::<_, i64>(3)?)?;
            Ok(TokenSourceUsage {
                source: normalized_app_type(&row.get::<_, String>(0)?),
                request_count: nonnegative_u64(row.get::<_, i64>(1)?)?,
                input_tokens,
                output_tokens,
                cache_read_tokens: nonnegative_u64(row.get::<_, i64>(4)?)?,
                cache_creation_tokens: nonnegative_u64(row.get::<_, i64>(5)?)?,
                reported_total_tokens: input_tokens.saturating_add(output_tokens),
            })
        })
        .map_err(|_| JournalFetchError::new("cc_switch_usage_query_failed", true))?;
    let mut by_source = Vec::new();
    for row in rows {
        by_source.push(row.map_err(|_| JournalFetchError::new("cc_switch_usage_invalid", false))?);
    }

    let last_synced_at_ms = database
        .query_row(
            "SELECT MAX(last_synced_at) FROM session_log_sync",
            [],
            |row| row.get::<_, Option<i64>>(0),
        )
        .map_err(|_| JournalFetchError::new("cc_switch_sync_query_failed", true))?
        .and_then(|seconds| seconds.checked_mul(1_000));
    let semantic_count = database
        .query_row(
            "SELECT COUNT(DISTINCT input_token_semantics) FROM proxy_request_logs \
             WHERE created_at >= ?1 AND created_at < ?2",
            params![start_seconds, end_seconds],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|_| JournalFetchError::new("cc_switch_usage_query_failed", true))?;
    let now_ms = system_time_ms();
    let is_current_window = request.window_start_ms <= now_ms && now_ms < request.window_end_ms;
    let stale = is_current_window
        && last_synced_at_ms.is_none_or(|last_sync| now_ms - last_sync > TOKEN_SYNC_STALE_MS);

    let totals = by_source.iter().fold([0_u64; 4], |mut totals, usage| {
        totals[0] = totals[0].saturating_add(usage.input_tokens);
        totals[1] = totals[1].saturating_add(usage.output_tokens);
        totals[2] = totals[2].saturating_add(usage.cache_read_tokens);
        totals[3] = totals[3].saturating_add(usage.cache_creation_tokens);
        totals
    });
    let has_usage = !by_source.is_empty();
    let (state, reason) = if stale {
        (FactState::Degraded, "cc_switch_session_sync_stale")
    } else if semantic_count > 1 {
        (FactState::Degraded, "cc_switch_input_token_semantics_mixed")
    } else if has_usage {
        (FactState::Healthy, "cc_switch_usage_ready")
    } else {
        (FactState::Healthy, "cc_switch_no_usage_observed")
    };
    Ok(TokenUsage {
        state,
        reason: reason.to_owned(),
        window_start_ms: request.window_start_ms,
        window_end_ms: request.window_end_ms,
        last_synced_at_ms,
        input_tokens: has_usage.then_some(totals[0]),
        output_tokens: has_usage.then_some(totals[1]),
        cache_read_tokens: has_usage.then_some(totals[2]),
        cache_creation_tokens: has_usage.then_some(totals[3]),
        reported_total_tokens: has_usage.then_some(totals[0].saturating_add(totals[1])),
        total_method: if has_usage {
            "input_plus_output".to_owned()
        } else {
            "unavailable".to_owned()
        },
        by_source,
    })
}

fn nonnegative_u64(value: i64) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

fn normalized_app_type(value: &str) -> String {
    match value {
        "claude-desktop" => "claude".to_owned(),
        other => other.to_owned(),
    }
}

fn configured_sources(database: &Connection) -> rusqlite::Result<BTreeSet<String>> {
    let mut statement = database.prepare("SELECT DISTINCT app_type FROM providers")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut sources = BTreeSet::new();
    for row in rows {
        sources.insert(normalized_app_type(&row?));
    }
    Ok(sources)
}

fn tracked_paths(
    database: &Connection,
    paths: &CollectorPaths,
    request: &JournalFetchRequest,
) -> Result<Vec<TrackedPath>, JournalFetchError> {
    let mut statement = database
        .prepare("SELECT file_path, last_modified FROM session_log_sync")
        .map_err(|_| JournalFetchError::new("cc_switch_session_index_unavailable", true))?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(|_| JournalFetchError::new("cc_switch_session_index_unavailable", true))?;
    let mut entries = Vec::new();
    let mut seen = BTreeSet::new();
    for row in rows {
        let (raw_path, raw_modified) =
            row.map_err(|_| JournalFetchError::new("cc_switch_session_index_invalid", false))?;
        let Some(updated_at_ms) = normalize_epoch_ms(raw_modified) else {
            continue;
        };
        if updated_at_ms < request.window_start_ms || updated_at_ms >= request.window_end_ms {
            continue;
        }
        let (source, path) =
            if raw_path.starts_with(paths.codex_sessions.to_string_lossy().as_ref()) {
                (SessionSource::Codex, PathBuf::from(raw_path))
            } else if raw_path.starts_with(paths.claude_projects.to_string_lossy().as_ref()) {
                (SessionSource::Claude, PathBuf::from(raw_path))
            } else if raw_path.starts_with(paths.opencode_db.to_string_lossy().as_ref()) {
                (SessionSource::OpenCode, paths.opencode_db.clone())
            } else {
                continue;
            };
        if source != SessionSource::OpenCode && !seen.insert(path.clone()) {
            continue;
        }
        entries.push(TrackedPath {
            source,
            path,
            updated_at_ms,
        });
    }
    Ok(entries)
}

fn normalize_epoch_ms(value: i64) -> Option<i64> {
    if value < 0 {
        None
    } else if value >= 100_000_000_000_000_000 {
        Some(value / 1_000_000)
    } else if value >= 100_000_000_000 {
        Some(value)
    } else {
        value.checked_mul(1_000)
    }
}

fn parse_codex_session(
    path: &Path,
    updated_at_ms: i64,
    allowed_root: &Path,
) -> Result<NormalizedSession, String> {
    validate_session_file(path, allowed_root)?;
    let reader = BufReader::new(File::open(path).map_err(|_| "session_file_unreadable")?);
    let mut messages = Vec::new();
    let mut session_id = None;
    let mut workspace = None;
    for line in reader.lines() {
        let line = line.map_err(|_| "session_file_unreadable")?;
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) == Some("session_meta") {
            let payload = &value["payload"];
            session_id = payload
                .get("id")
                .or_else(|| payload.get("session_id"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            workspace = payload
                .get("cwd")
                .and_then(Value::as_str)
                .map(str::to_owned);
            continue;
        }
        if value.get("type").and_then(Value::as_str) != Some("response_item")
            || value.pointer("/payload/type").and_then(Value::as_str) != Some("message")
        {
            continue;
        }
        let Some(role) = value.pointer("/payload/role").and_then(Value::as_str) else {
            continue;
        };
        if role != "user" && role != "assistant" {
            continue;
        }
        let text = value
            .pointer("/payload/content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|item| {
                matches!(
                    item.get("type").and_then(Value::as_str),
                    Some("input_text" | "output_text" | "text")
                )
            })
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        push_message(&mut messages, role, &text);
    }
    let fallback = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("codex-session");
    Ok(finalize_session(
        SessionSource::Codex,
        session_id.unwrap_or_else(|| fallback.to_owned()),
        workspace,
        updated_at_ms,
        messages,
        None,
    ))
}

fn parse_claude_session(
    path: &Path,
    updated_at_ms: i64,
    allowed_root: &Path,
) -> Result<NormalizedSession, String> {
    validate_session_file(path, allowed_root)?;
    let reader = BufReader::new(File::open(path).map_err(|_| "session_file_unreadable")?);
    let mut messages = Vec::new();
    let mut session_id = None;
    let mut workspace = None;
    for line in reader.lines() {
        let line = line.map_err(|_| "session_file_unreadable")?;
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(role) = value.pointer("/message/role").and_then(Value::as_str) else {
            continue;
        };
        if role != "user" && role != "assistant" {
            continue;
        }
        session_id = session_id.or_else(|| {
            value
                .get("sessionId")
                .or_else(|| value.get("session_id"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
        workspace =
            workspace.or_else(|| value.get("cwd").and_then(Value::as_str).map(str::to_owned));
        let content = value.pointer("/message/content");
        let text = match content {
            Some(Value::String(text)) => text.clone(),
            Some(Value::Array(items)) => items
                .iter()
                .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        push_message(&mut messages, role, &text);
    }
    let fallback = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("claude-session");
    Ok(finalize_session(
        SessionSource::Claude,
        session_id.unwrap_or_else(|| fallback.to_owned()),
        workspace,
        updated_at_ms,
        messages,
        None,
    ))
}

fn collect_opencode_sessions(
    path: &Path,
    request: &JournalFetchRequest,
) -> Result<Vec<NormalizedSession>, String> {
    validate_owned_regular_file(path, "opencode_database_unavailable")
        .map_err(|error| error.reason)?;
    let database = open_read_only(path).map_err(|_| "opencode_database_unavailable")?;
    let mut statement = database
        .prepare(
            "SELECT id, title, directory, time_updated FROM session \
             WHERE time_updated >= ?1 AND time_updated < ?2 \
             ORDER BY time_updated DESC, id LIMIT ?3",
        )
        .map_err(|_| "opencode_session_query_failed")?;
    let rows = statement
        .query_map(
            params![
                request.window_start_ms,
                request.window_end_ms,
                MAX_SESSIONS as i64
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .map_err(|_| "opencode_session_query_failed")?;
    let mut summaries = Vec::new();
    for row in rows {
        summaries.push(row.map_err(|_| "opencode_session_invalid")?);
    }
    drop(statement);

    let mut sessions = Vec::new();
    for (session_id, title, directory, updated_at_ms) in summaries {
        let mut message_statement = database
            .prepare(
                "SELECT m.data, p.data FROM message m JOIN part p ON p.message_id = m.id \
                 WHERE m.session_id = ?1 ORDER BY m.time_created, p.time_created, p.id",
            )
            .map_err(|_| "opencode_message_query_failed")?;
        let rows = message_statement
            .query_map([&session_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|_| "opencode_message_query_failed")?;
        let mut messages = Vec::new();
        for row in rows {
            let (message_json, part_json) = row.map_err(|_| "opencode_message_invalid")?;
            let Ok(message) = serde_json::from_str::<Value>(&message_json) else {
                continue;
            };
            let Ok(part) = serde_json::from_str::<Value>(&part_json) else {
                continue;
            };
            let Some(role) = message.get("role").and_then(Value::as_str) else {
                continue;
            };
            if role != "user" && role != "assistant" {
                continue;
            }
            if part.get("type").and_then(Value::as_str) != Some("text") {
                continue;
            }
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                push_message(&mut messages, role, text);
            }
        }
        sessions.push(finalize_session(
            SessionSource::OpenCode,
            session_id,
            Some(directory),
            updated_at_ms,
            messages,
            Some(title),
        ));
    }
    Ok(sessions)
}

fn validate_session_file(path: &Path, allowed_root: &Path) -> Result<(), String> {
    if !path.starts_with(allowed_root) {
        return Err("session_path_outside_root".to_owned());
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| "session_file_unreadable")?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != Uid::effective().as_raw()
        || metadata.len() > MAX_SOURCE_FILE_BYTES
    {
        return Err("session_file_unsafe".to_owned());
    }
    Ok(())
}

fn push_message(messages: &mut Vec<NormalizedMessage>, role: &str, raw_text: &str) {
    let Some(content) = sanitize_text(raw_text) else {
        return;
    };
    if messages
        .last()
        .is_some_and(|previous| previous.role == role && previous.content == content)
    {
        return;
    }
    messages.push(NormalizedMessage {
        role: role.to_owned(),
        content,
    });
}

fn sanitize_text(raw_text: &str) -> Option<String> {
    let text = raw_text.trim();
    if text.is_empty()
        || [
            "<codex_internal_context",
            "<environment_context",
            "<recommended_plugins",
            "# AGENTS.md instructions",
            "<permissions instructions",
        ]
        .iter()
        .any(|prefix| text.starts_with(prefix))
    {
        return None;
    }
    let mut output = String::new();
    for line in text.lines() {
        let lowercase = line.to_ascii_lowercase();
        let sensitive = lowercase.contains("authorization:")
            || lowercase.contains("api_key=")
            || lowercase.contains("api-key:")
            || lowercase.contains("password=")
            || lowercase.contains("\"access_token\"")
            || lowercase.contains("\"refresh_token\"")
            || lowercase.contains("begin private key");
        let line = if sensitive {
            "[redacted sensitive line]"
        } else {
            line
        };
        if !output.is_empty() {
            output.push('\n');
        }
        append_bounded(&mut output, line, MAX_MESSAGE_TEXT_CHARS);
        if output.chars().count() >= MAX_MESSAGE_TEXT_CHARS {
            break;
        }
    }
    (!output.trim().is_empty()).then_some(output)
}

fn append_bounded(output: &mut String, value: &str, max_chars: usize) {
    let remaining = max_chars.saturating_sub(output.chars().count());
    output.extend(value.chars().take(remaining));
}

fn finalize_session(
    source: SessionSource,
    session_id: String,
    workspace: Option<String>,
    updated_at_ms: i64,
    mut messages: Vec<NormalizedMessage>,
    explicit_title: Option<String>,
) -> NormalizedSession {
    let mut retained_chars = 0_usize;
    messages.retain(|message| {
        if retained_chars >= MAX_SESSION_TEXT_CHARS {
            return false;
        }
        retained_chars = retained_chars.saturating_add(message.content.chars().count());
        true
    });
    if retained_chars > MAX_SESSION_TEXT_CHARS {
        if let Some(last) = messages.last_mut() {
            let overflow = retained_chars - MAX_SESSION_TEXT_CHARS;
            let keep = last.content.chars().count().saturating_sub(overflow);
            last.content = last.content.chars().take(keep).collect();
        }
        retained_chars = MAX_SESSION_TEXT_CHARS;
    }
    let substantive_messages = u32::try_from(messages.len()).unwrap_or(u32::MAX);
    let included =
        substantive_messages >= SHORT_SESSION_MESSAGES || retained_chars >= SHORT_SESSION_CHARS;
    let length_class =
        if substantive_messages >= LONG_SESSION_MESSAGES || retained_chars >= LONG_SESSION_CHARS {
            "long"
        } else if included {
            "normal"
        } else {
            "short"
        };
    let title = explicit_title
        .filter(|title| !title.trim().is_empty())
        .or_else(|| {
            messages
                .iter()
                .find(|message| message.role == "user")
                .map(|message| message.content.lines().next().unwrap_or("会话"))
                .map(|line| line.chars().take(120).collect())
        })
        .unwrap_or_else(|| "未命名会话".to_owned());
    NormalizedSession {
        source: source.name().to_owned(),
        session_id,
        title,
        workspace,
        updated_at_ms,
        eligibility: SessionEligibility {
            state: if included {
                EligibilityState::Included
            } else {
                EligibilityState::IgnoredShort
            },
            reason: if included {
                "meets_default_threshold"
            } else {
                "below_default_threshold"
            }
            .to_owned(),
            substantive_messages,
            content_chars: u32::try_from(retained_chars).unwrap_or(u32::MAX),
            length_class: length_class.to_owned(),
        },
        messages,
    }
}

fn enforce_corpus_text_limit(sessions: &mut [NormalizedSession], warnings: &mut Vec<String>) {
    let mut retained = 0_usize;
    let mut truncated = false;
    for session in sessions {
        for message in &mut session.messages {
            let chars = message.content.chars().count();
            if retained >= MAX_TOTAL_TEXT_CHARS {
                message.content.clear();
                truncated = true;
                continue;
            }
            let keep = (MAX_TOTAL_TEXT_CHARS - retained).min(chars);
            if keep < chars {
                message.content = message.content.chars().take(keep).collect();
                truncated = true;
            }
            retained += keep;
        }
        session
            .messages
            .retain(|message| !message.content.is_empty());
    }
    if truncated {
        warnings.push("session_corpus_text_limit_reached".to_owned());
    }
}

fn system_time_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn request() -> JournalFetchRequest {
        JournalFetchRequest {
            local_date: "2026-08-20".to_owned(),
            timezone: "Asia/Shanghai".to_owned(),
            window_start_ms: 1_787_155_200_000,
            window_end_ms: 1_787_241_600_000,
        }
    }

    fn fixture_paths(temp: &TempDir) -> CollectorPaths {
        let home = temp.path().to_path_buf();
        CollectorPaths {
            cc_switch_db: home.join(".cc-switch/cc-switch.db"),
            codex_sessions: home.join(".codex/sessions"),
            claude_projects: home.join(".claude/projects"),
            opencode_db: home.join(".local/share/opencode/opencode.db"),
            home,
        }
    }

    fn create_cc_switch_database(paths: &CollectorPaths) -> Connection {
        fs::create_dir_all(paths.cc_switch_db.parent().unwrap()).unwrap();
        let connection = Connection::open(&paths.cc_switch_db).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE providers (app_type TEXT NOT NULL);\
                 CREATE TABLE session_log_sync (\
                   file_path TEXT PRIMARY KEY, last_modified INTEGER NOT NULL,\
                   last_line_offset INTEGER NOT NULL DEFAULT 0, last_synced_at INTEGER NOT NULL\
                 );\
                 CREATE TABLE proxy_request_logs (\
                   app_type TEXT NOT NULL, created_at INTEGER NOT NULL,\
                   input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL,\
                   cache_read_tokens INTEGER NOT NULL, cache_creation_tokens INTEGER NOT NULL,\
                   input_token_semantics INTEGER NOT NULL DEFAULT 0\
                 );",
            )
            .unwrap();
        connection
    }

    #[test]
    fn request_rejects_invalid_date_timezone_and_window() {
        let mut value = request();
        value.local_date = "2026-8-20".to_owned();
        assert_eq!(
            validate_request(&value).unwrap_err().reason,
            "journal_local_date_invalid"
        );
        let mut value = request();
        value.timezone = "bad\ntimezone".to_owned();
        assert_eq!(
            validate_request(&value).unwrap_err().reason,
            "journal_timezone_invalid"
        );
        let mut value = request();
        value.window_end_ms = value.window_start_ms + 1_000;
        assert_eq!(
            validate_request(&value).unwrap_err().reason,
            "journal_time_window_invalid"
        );
    }

    #[test]
    fn epoch_normalization_handles_seconds_millis_and_nanos() {
        assert_eq!(normalize_epoch_ms(1_787_200_000), Some(1_787_200_000_000));
        assert_eq!(
            normalize_epoch_ms(1_787_200_000_123),
            Some(1_787_200_000_123)
        );
        assert_eq!(
            normalize_epoch_ms(1_787_200_000_123_456_789),
            Some(1_787_200_000_123)
        );
        assert_eq!(normalize_epoch_ms(-1), None);
    }

    #[test]
    fn sanitization_drops_bootstrap_and_redacts_secret_lines() {
        assert_eq!(
            sanitize_text("<environment_context>secret</environment_context>"),
            None
        );
        assert_eq!(
            sanitize_text("keep\nAuthorization: Bearer secret\nend").unwrap(),
            "keep\n[redacted sensitive line]\nend"
        );
    }

    #[test]
    fn collector_reads_cc_switch_tokens_and_filters_short_sessions() {
        let temp = TempDir::new().unwrap();
        let paths = fixture_paths(&temp);
        fs::create_dir_all(&paths.codex_sessions).unwrap();
        fs::create_dir_all(&paths.claude_projects).unwrap();
        fs::create_dir_all(paths.opencode_db.parent().unwrap()).unwrap();
        let codex_path = paths.codex_sessions.join("codex.jsonl");
        let claude_path = paths.claude_projects.join("claude.jsonl");
        fs::write(
            &codex_path,
            concat!(
                "{\"type\":\"session_meta\",\"payload\":{\"id\":\"codex-1\",\"cwd\":\"/work\"}}\n",
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"实现日志采集\"}]}}\n",
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"开始读取 cc-switch 会话索引\"}]}}\n",
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"只读打开数据库\"}]}}\n",
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"已完成只读查询\"}]}}\n"
            ),
        )
        .unwrap();
        fs::write(
            &claude_path,
            concat!(
                "{\"type\":\"user\",\"sessionId\":\"claude-1\",\"cwd\":\"/work\",\"message\":{\"role\":\"user\",\"content\":\"你好\"}}\n",
                "{\"type\":\"assistant\",\"sessionId\":\"claude-1\",\"cwd\":\"/work\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"你好\"}]}}\n"
            ),
        )
        .unwrap();
        let database = create_cc_switch_database(&paths);
        for source in ["codex", "claude", "opencode"] {
            database
                .execute("INSERT INTO providers(app_type) VALUES (?1)", [source])
                .unwrap();
        }
        database
            .execute(
                "INSERT INTO session_log_sync(file_path,last_modified,last_synced_at) VALUES (?1,?2,?3)",
                params![codex_path.to_string_lossy(), 1_787_200_000_000_i64, 1_787_200_000_i64],
            )
            .unwrap();
        database
            .execute(
                "INSERT INTO session_log_sync(file_path,last_modified,last_synced_at) VALUES (?1,?2,?3)",
                params![claude_path.to_string_lossy(), 1_787_200_100_000_i64, 1_787_200_100_i64],
            )
            .unwrap();
        database
            .execute(
                "INSERT INTO proxy_request_logs(app_type,created_at,input_tokens,output_tokens,cache_read_tokens,cache_creation_tokens,input_token_semantics) VALUES ('codex',1787200000,100,25,50,0,0)",
                [],
            )
            .unwrap();
        drop(database);

        let corpus = JournalCollector::new(paths).collect(&request()).unwrap();
        assert_eq!(corpus.schema_version, 1);
        assert_eq!(corpus.token_usage.reported_total_tokens, Some(125));
        assert_eq!(corpus.token_usage.by_source[0].source, "codex");
        assert_eq!(corpus.sessions.len(), 2);
        assert_eq!(corpus.sessions[0].source, "claude");
        assert_eq!(
            corpus.sessions[0].eligibility.state,
            EligibilityState::IgnoredShort
        );
        assert_eq!(corpus.sessions[1].source, "codex");
        assert_eq!(
            corpus.sessions[1].eligibility.state,
            EligibilityState::Included
        );
        let opencode = corpus
            .source_coverage
            .iter()
            .find(|coverage| coverage.source == "opencode")
            .unwrap();
        assert_eq!(opencode.state, FactState::Degraded);
        assert_eq!(opencode.reason, "cc_switch_session_source_not_tracked");
    }

    #[test]
    fn path_validation_rejects_session_outside_registered_root() {
        let temp = TempDir::new().unwrap();
        let outside = temp.path().join("outside.jsonl");
        let allowed = temp.path().join("allowed");
        fs::create_dir_all(&allowed).unwrap();
        fs::write(&outside, "{}\n").unwrap();
        assert_eq!(
            validate_session_file(&outside, &allowed).unwrap_err(),
            "session_path_outside_root"
        );
    }

    #[test]
    fn environment_paths_stay_under_home() {
        let paths = CollectorPaths::from_environment().unwrap();
        assert!(paths.cc_switch_db.starts_with(&paths.home));
        assert!(paths.codex_sessions.starts_with(&paths.home));
        assert!(paths.claude_projects.starts_with(&paths.home));
        assert!(paths.opencode_db.starts_with(&paths.home));
    }

    #[test]
    fn production_collector_constructor_resolves_paths_without_collecting() {
        let collector = JournalCollector::from_environment().unwrap();
        assert!(
            collector
                .paths
                .cc_switch_db
                .starts_with(&collector.paths.home)
        );
    }
}
