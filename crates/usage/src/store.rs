use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{
    Connection, InterruptHandle, OpenFlags, OptionalExtension, Transaction, TransactionBehavior,
    params,
};
use thiserror::Error;

use crate::{
    BucketSlice, ClockSample, MAX_NIRI_APP_ID_BYTES, SummaryBucket, SummaryKind, WindowIdentity,
};

const DAY_MS: i64 = 86_400_000;
const DAILY_COVERAGE_BOUNDARY_DAYS: u32 = 3;
const WEEKLY_COVERAGE_BOUNDARY_DAYS: u32 = 9;
static BACKUP_NONCE: AtomicU64 = AtomicU64::new(1);
pub const USAGE_STORE_SCHEMA_VERSION: u32 = 2;
pub const MAX_SUMMARY_APPLICATIONS: usize = 1_024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AggregateKind {
    Minute,
    Daily,
    Weekly,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AggregateEntry {
    pub app_id: String,
    pub duration_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AggregateQuery {
    pub entries: Vec<AggregateEntry>,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentedAggregateEntry {
    pub app_id: String,
    pub bucket_key: String,
    pub timezone_id: String,
    pub utc_offset_seconds: i32,
    pub bucket_start_utc_ms: i64,
    pub last_wall_utc_ms: i64,
    pub duration_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentedAggregateQuery {
    pub entries: Vec<SegmentedAggregateEntry>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UsageCoverageState {
    pub event_gap_count: u64,
    pub recovered_interval_count: u64,
    pub last_gap_wall_utc_ms: Option<i64>,
    pub last_gap_reason: Option<String>,
    pub tracking_started_wall_utc_ms: Option<i64>,
    pub tracking_start_daily_key: Option<String>,
    pub tracking_start_weekly_key: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetentionPolicy {
    pub raw_days: u32,
    pub minute_days: u32,
    pub daily_days: u32,
    pub weekly_weeks: u32,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            raw_days: 30,
            minute_days: 30,
            daily_days: 400,
            weekly_weeks: 260,
        }
    }
}

#[derive(Debug)]
pub struct UsageStore {
    connection: Connection,
    writer: WriterIdentity,
}

pub struct UsageReader {
    connection: Connection,
    interrupt: UsageQueryInterrupt,
}

#[derive(Clone)]
pub struct UsageQueryInterrupt {
    handle: Arc<InterruptHandle>,
}

impl UsageQueryInterrupt {
    pub fn interrupt(&self) {
        self.handle.interrupt();
    }
}

impl UsageReader {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, UsageStoreError> {
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.busy_timeout(std::time::Duration::from_secs(2))?;
        ensure_database_integrity(&connection)?;
        ensure_reader_schema(&connection)?;
        connection.execute_batch("PRAGMA query_only = ON;")?;
        let interrupt = UsageQueryInterrupt {
            handle: Arc::new(connection.get_interrupt_handle()),
        };
        Ok(Self {
            connection,
            interrupt,
        })
    }

    pub fn interrupt_handle(&self) -> UsageQueryInterrupt {
        self.interrupt.clone()
    }

    pub fn aggregate_segments_for_key(
        &self,
        kind: SummaryKind,
        bucket_key: &str,
    ) -> Result<SegmentedAggregateQuery, UsageStoreError> {
        query_aggregate_segments_for_key(&self.connection, kind, bucket_key)
    }

    pub fn coverage_state(&self) -> Result<UsageCoverageState, UsageStoreError> {
        query_coverage_state(&self.connection)
    }

    pub fn coverage_state_for_bucket(
        &self,
        kind: SummaryKind,
        bucket_key: &str,
    ) -> Result<UsageCoverageState, UsageStoreError> {
        query_coverage_state_for_bucket(&self.connection, kind, bucket_key)
    }
}

impl UsageStore {
    pub fn open(
        path: impl AsRef<Path>,
        recovered_at: &ClockSample,
    ) -> Result<Self, UsageStoreError> {
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(std::time::Duration::from_secs(2))?;
        ensure_database_integrity(&connection)?;
        migrate_usage_schema(&mut connection, recovered_at)?;
        let writer = WriterIdentity::current()?;
        acquire_writer_lease(&mut connection, &writer, recovered_at.wall_utc_ms)?;
        let mut store = Self { connection, writer };
        store.recover_open_intervals(recovered_at)?;
        Ok(store)
    }

    pub fn start_interval(
        &mut self,
        identity: &WindowIdentity,
        start: &ClockSample,
    ) -> Result<i64, UsageStoreError> {
        if identity.app_id.is_empty()
            || identity.app_id.len() > MAX_NIRI_APP_ID_BYTES
            || identity.app_id.contains('\0')
        {
            return Err(UsageStoreError::InvalidAppId);
        }
        let monotonic_ns = sql_u64(start.monotonic_ns)?;
        self.connection.execute(
            "INSERT INTO focus_intervals (
                app_id, pid, window_id, boot_id,
                started_monotonic_ns, last_checkpoint_monotonic_ns,
                started_wall_utc_ms, last_checkpoint_wall_utc_ms,
                start_timezone_id, start_utc_offset_seconds,
                duration_ns, state
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?6, ?7, ?8, 0, 'open')",
            params![
                identity.app_id,
                identity.pid,
                sql_u64(identity.window_id)?,
                start.boot_id,
                monotonic_ns,
                start.wall_utc_ms,
                start.timezone_id,
                start.utc_offset_seconds,
            ],
        )?;
        Ok(self.connection.last_insert_rowid())
    }

    pub fn append_segment(
        &mut self,
        interval_id: i64,
        app_id: &str,
        end: &ClockSample,
        slices: &[BucketSlice],
    ) -> Result<(), UsageStoreError> {
        let duration_ns = slices.iter().try_fold(0_u64, |total, slice| {
            total
                .checked_add(slice.duration_ns)
                .ok_or(UsageStoreError::NumericOverflow)
        })?;
        if duration_ns == 0 {
            return Ok(());
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = transaction.execute(
            "UPDATE focus_intervals
             SET duration_ns = duration_ns + ?1,
                 last_checkpoint_monotonic_ns = ?2,
                 last_checkpoint_wall_utc_ms = ?3
             WHERE id = ?4 AND state = 'open'",
            params![
                sql_u64(duration_ns)?,
                sql_u64(end.monotonic_ns)?,
                end.wall_utc_ms,
                interval_id,
            ],
        )?;
        if updated != 1 {
            return Err(UsageStoreError::IntervalNotOpen(interval_id));
        }
        for slice in slices {
            upsert_aggregate(
                &transaction,
                "minute_aggregates",
                app_id,
                &slice.minute_key,
                slice,
                slice.bucket_start_utc_ms,
            )?;
            upsert_aggregate(
                &transaction,
                "daily_aggregates",
                app_id,
                &slice.day_key,
                slice,
                slice.day_start_utc_ms,
            )?;
            upsert_aggregate(
                &transaction,
                "weekly_aggregates",
                app_id,
                &slice.week_key,
                slice,
                slice.week_start_utc_ms,
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn close_interval(
        &mut self,
        interval_id: i64,
        end: &ClockSample,
        reason: &str,
    ) -> Result<(), UsageStoreError> {
        self.connection.execute(
            "UPDATE focus_intervals
             SET ended_monotonic_ns = last_checkpoint_monotonic_ns,
                 ended_wall_utc_ms = last_checkpoint_wall_utc_ms,
                 end_timezone_id = ?1,
                 end_utc_offset_seconds = ?2,
                 state = 'closed',
                 end_reason = ?3
             WHERE id = ?4 AND state = 'open'",
            params![end.timezone_id, end.utc_offset_seconds, reason, interval_id],
        )?;
        Ok(())
    }

    pub fn apply_retention(
        &mut self,
        now_wall_utc_ms: i64,
        policy: RetentionPolicy,
    ) -> Result<(), UsageStoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "DELETE FROM focus_intervals
             WHERE state != 'open'
               AND COALESCE(ended_wall_utc_ms, last_checkpoint_wall_utc_ms) < ?1",
            [cutoff_days(now_wall_utc_ms, policy.raw_days)?],
        )?;
        delete_older_than(
            &transaction,
            "minute_aggregates",
            "last_wall_utc_ms",
            cutoff_days(now_wall_utc_ms, policy.minute_days)?,
        )?;
        delete_older_than(
            &transaction,
            "daily_aggregates",
            "last_wall_utc_ms",
            cutoff_days(now_wall_utc_ms, policy.daily_days)?,
        )?;
        let weekly_days = policy
            .weekly_weeks
            .checked_mul(7)
            .ok_or(UsageStoreError::NumericOverflow)?;
        delete_older_than(
            &transaction,
            "weekly_aggregates",
            "last_wall_utc_ms",
            cutoff_days(now_wall_utc_ms, weekly_days)?,
        )?;
        // Aggregate retention compares the last sample inside a local bucket.
        // Keep gaps through the full boundary day/week so a retained aggregate
        // can never outlive the coverage fact that makes it partial. The extra
        // offset span covers any FixedOffset accepted by chrono, including a
        // timezone change between opposite UTC offsets inside one bucket.
        let daily_coverage_days = policy
            .daily_days
            .checked_add(DAILY_COVERAGE_BOUNDARY_DAYS)
            .ok_or(UsageStoreError::NumericOverflow)?;
        let weekly_coverage_days = weekly_days
            .checked_add(WEEKLY_COVERAGE_BOUNDARY_DAYS)
            .ok_or(UsageStoreError::NumericOverflow)?;
        let coverage_days = daily_coverage_days.max(weekly_coverage_days);
        transaction.execute(
            "DELETE FROM coverage_gaps
             WHERE ended_wall_utc_ms IS NOT NULL AND ended_wall_utc_ms < ?1",
            [cutoff_days(now_wall_utc_ms, coverage_days)?],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn aggregate_duration_ns(
        &self,
        kind: AggregateKind,
        app_id: &str,
        bucket_key: &str,
    ) -> Result<u64, UsageStoreError> {
        let table = match kind {
            AggregateKind::Minute => "minute_aggregates",
            AggregateKind::Daily => "daily_aggregates",
            AggregateKind::Weekly => "weekly_aggregates",
        };
        let query = format!(
            "SELECT COALESCE(SUM(duration_ns), 0) FROM {table} WHERE app_id = ?1 AND bucket_key = ?2"
        );
        let value: i64 = self
            .connection
            .query_row(&query, params![app_id, bucket_key], |row| row.get(0))?;
        u64::try_from(value).map_err(|_| UsageStoreError::NumericOverflow)
    }

    /// Returns one timezone/offset segment of a daily or weekly bucket.
    ///
    /// Results are deterministic and bounded. `truncated` is explicit instead of
    /// silently implying complete coverage when more than 1024 applications exist.
    pub fn aggregates_for_bucket(
        &self,
        bucket: &SummaryBucket,
    ) -> Result<AggregateQuery, UsageStoreError> {
        query_aggregates_for_bucket(&self.connection, bucket)
    }

    /// Returns every timezone/offset segment for one typed daily or weekly key.
    /// Rows are never folded across timezone changes.
    pub fn aggregate_segments_for_key(
        &self,
        kind: SummaryKind,
        bucket_key: &str,
    ) -> Result<SegmentedAggregateQuery, UsageStoreError> {
        query_aggregate_segments_for_key(&self.connection, kind, bucket_key)
    }

    pub fn begin_gap(&mut self, reason: &str, start: &ClockSample) -> Result<(), UsageStoreError> {
        validate_gap_reason(reason)?;
        let (daily_key, weekly_key) = gap_bucket_keys(start)?;
        self.connection.execute(
            "INSERT INTO coverage_gaps (
                started_wall_utc_ms, start_daily_key, start_weekly_key, reason
             ) SELECT ?1, ?2, ?3, ?4
             WHERE NOT EXISTS (SELECT 1 FROM coverage_gaps WHERE ended_wall_utc_ms IS NULL)",
            params![start.wall_utc_ms, daily_key, weekly_key, reason],
        )?;
        Ok(())
    }

    pub fn close_open_gaps(&mut self, end: &ClockSample) -> Result<(), UsageStoreError> {
        let (daily_key, weekly_key) = gap_bucket_keys(end)?;
        self.connection.execute(
            "UPDATE coverage_gaps
             SET ended_wall_utc_ms = MAX(started_wall_utc_ms, ?1),
                 end_daily_key = ?2,
                 end_weekly_key = ?3
             WHERE ended_wall_utc_ms IS NULL",
            params![end.wall_utc_ms, daily_key, weekly_key],
        )?;
        Ok(())
    }

    pub fn record_gap_interval(
        &mut self,
        reason: &str,
        start: &ClockSample,
        end: &ClockSample,
    ) -> Result<(), UsageStoreError> {
        validate_gap_reason(reason)?;
        let (start_daily, start_weekly) = gap_bucket_keys(start)?;
        let (end_daily, end_weekly) = gap_bucket_keys(end)?;
        self.connection.execute(
            "INSERT INTO coverage_gaps (
                started_wall_utc_ms, ended_wall_utc_ms,
                start_daily_key, end_daily_key, start_weekly_key, end_weekly_key, reason
             ) VALUES (?1, MAX(?1, ?2), ?3, ?4, ?5, ?6, ?7)",
            params![
                start.wall_utc_ms,
                end.wall_utc_ms,
                start_daily,
                end_daily,
                start_weekly,
                end_weekly,
                reason,
            ],
        )?;
        Ok(())
    }

    pub fn coverage_state(&self) -> Result<UsageCoverageState, UsageStoreError> {
        query_coverage_state(&self.connection)
    }

    pub fn coverage_state_for_bucket(
        &self,
        kind: SummaryKind,
        bucket_key: &str,
    ) -> Result<UsageCoverageState, UsageStoreError> {
        query_coverage_state_for_bucket(&self.connection, kind, bucket_key)
    }

    pub fn has_open_gap(&self) -> Result<bool, UsageStoreError> {
        let count: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM coverage_gaps WHERE ended_wall_utc_ms IS NULL",
            [],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn interval_count(&self) -> Result<u64, UsageStoreError> {
        let value: i64 =
            self.connection
                .query_row("SELECT COUNT(*) FROM focus_intervals", [], |row| row.get(0))?;
        u64::try_from(value).map_err(|_| UsageStoreError::NumericOverflow)
    }

    pub fn open_interval_count(&self) -> Result<u64, UsageStoreError> {
        let value: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM focus_intervals WHERE state = 'open'",
            [],
            |row| row.get(0),
        )?;
        u64::try_from(value).map_err(|_| UsageStoreError::NumericOverflow)
    }

    fn recover_open_intervals(
        &mut self,
        recovered_at: &ClockSample,
    ) -> Result<usize, UsageStoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let earliest_checkpoint: Option<i64> = transaction.query_row(
            "SELECT MIN(last_checkpoint_wall_utc_ms) FROM focus_intervals WHERE state = 'open'",
            [],
            |row| row.get(0),
        )?;
        let recovered = transaction.execute(
            "UPDATE focus_intervals
             SET ended_monotonic_ns = last_checkpoint_monotonic_ns,
                 ended_wall_utc_ms = last_checkpoint_wall_utc_ms,
                 state = 'recovered',
                 end_reason = 'crash_recovery',
                 recovered_at_wall_utc_ms = ?1
             WHERE state = 'open'",
            [recovered_at.wall_utc_ms],
        )?;
        if recovered > 0 {
            let started_wall_utc_ms = earliest_checkpoint.unwrap_or(recovered_at.wall_utc_ms);
            let start_sample = ClockSample {
                wall_utc_ms: started_wall_utc_ms,
                ..recovered_at.clone()
            };
            let (start_daily, start_weekly) = gap_bucket_keys(&start_sample)?;
            transaction.execute(
                "UPDATE coverage_gaps
                 SET recovered_interval_count = recovered_interval_count + ?1
                 WHERE ended_wall_utc_ms IS NULL",
                [i64::try_from(recovered).map_err(|_| UsageStoreError::NumericOverflow)?],
            )?;
            transaction.execute(
                "INSERT INTO coverage_gaps (
                    started_wall_utc_ms, start_daily_key, start_weekly_key,
                    reason, recovered_interval_count
                 ) SELECT ?1, ?2, ?3, 'crash_recovery', ?4
                 WHERE NOT EXISTS (
                    SELECT 1 FROM coverage_gaps WHERE ended_wall_utc_ms IS NULL
                 )",
                params![
                    started_wall_utc_ms,
                    start_daily,
                    start_weekly,
                    i64::try_from(recovered).map_err(|_| UsageStoreError::NumericOverflow)?,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(recovered)
    }
}

impl Drop for UsageStore {
    fn drop(&mut self) {
        let _ = self.connection.execute(
            "DELETE FROM writer_lease
             WHERE singleton = 1 AND owner_pid = ?1 AND owner_start_ticks = ?2 AND owner_boot_id = ?3",
            params![
                i64::from(self.writer.pid),
                sql_u64(self.writer.start_ticks).ok(),
                self.writer.boot_id,
            ],
        );
    }
}

fn query_aggregates_for_bucket(
    connection: &Connection,
    bucket: &SummaryBucket,
) -> Result<AggregateQuery, UsageStoreError> {
    let table = match bucket.kind {
        SummaryKind::Daily => "daily_aggregates",
        SummaryKind::Weekly => "weekly_aggregates",
    };
    let query = format!(
        "SELECT app_id, duration_ns FROM {table}
         WHERE bucket_key = ?1 AND timezone_id = ?2 AND utc_offset_seconds = ?3
           AND bucket_start_utc_ms = ?4
         ORDER BY duration_ns DESC, app_id ASC
         LIMIT ?5"
    );
    let mut statement = connection.prepare(&query)?;
    let rows = statement.query_map(
        params![
            bucket.bucket_key,
            bucket.timezone_id,
            bucket.utc_offset_seconds,
            bucket.bucket_start_utc_ms,
            i64::try_from(MAX_SUMMARY_APPLICATIONS + 1)
                .map_err(|_| UsageStoreError::NumericOverflow)?,
        ],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
    )?;
    let mut entries = Vec::with_capacity(MAX_SUMMARY_APPLICATIONS + 1);
    for row in rows {
        let (app_id, duration_ns) = row?;
        validate_stored_app_id(&app_id)?;
        entries.push(AggregateEntry {
            app_id,
            duration_ns: u64::try_from(duration_ns)
                .map_err(|_| UsageStoreError::NumericOverflow)?,
        });
    }
    let truncated = entries.len() > MAX_SUMMARY_APPLICATIONS;
    entries.truncate(MAX_SUMMARY_APPLICATIONS);
    Ok(AggregateQuery { entries, truncated })
}

fn query_aggregate_segments_for_key(
    connection: &Connection,
    kind: SummaryKind,
    bucket_key: &str,
) -> Result<SegmentedAggregateQuery, UsageStoreError> {
    validate_bucket_key(kind, bucket_key)?;
    let table = match kind {
        SummaryKind::Daily => "daily_aggregates",
        SummaryKind::Weekly => "weekly_aggregates",
    };
    let query = format!(
        "SELECT app_id, bucket_key, timezone_id, utc_offset_seconds,
                bucket_start_utc_ms, last_wall_utc_ms, duration_ns
         FROM {table}
         WHERE bucket_key = ?1
         ORDER BY duration_ns DESC, app_id ASC, timezone_id ASC,
                  utc_offset_seconds ASC, bucket_start_utc_ms ASC
         LIMIT ?2"
    );
    let mut statement = connection.prepare(&query)?;
    let rows = statement.query_map(
        params![
            bucket_key,
            i64::try_from(MAX_SUMMARY_APPLICATIONS + 1)
                .map_err(|_| UsageStoreError::NumericOverflow)?,
        ],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i32>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            ))
        },
    )?;
    let mut entries = Vec::with_capacity(MAX_SUMMARY_APPLICATIONS + 1);
    for row in rows {
        let (
            app_id,
            stored_bucket_key,
            timezone_id,
            utc_offset_seconds,
            bucket_start_utc_ms,
            last_wall_utc_ms,
            duration_ns,
        ) = row?;
        validate_stored_app_id(&app_id)?;
        if timezone_id.is_empty() || timezone_id.len() > 128 || timezone_id.contains('\0') {
            return Err(UsageStoreError::InvalidTimezoneId);
        }
        entries.push(SegmentedAggregateEntry {
            app_id,
            bucket_key: stored_bucket_key,
            timezone_id,
            utc_offset_seconds,
            bucket_start_utc_ms,
            last_wall_utc_ms,
            duration_ns: u64::try_from(duration_ns)
                .map_err(|_| UsageStoreError::NumericOverflow)?,
        });
    }
    let truncated = entries.len() > MAX_SUMMARY_APPLICATIONS;
    entries.truncate(MAX_SUMMARY_APPLICATIONS);
    Ok(SegmentedAggregateQuery { entries, truncated })
}

fn query_coverage_state(connection: &Connection) -> Result<UsageCoverageState, UsageStoreError> {
    query_coverage_state_filtered(connection, None)
}

fn query_coverage_state_for_bucket(
    connection: &Connection,
    kind: SummaryKind,
    bucket_key: &str,
) -> Result<UsageCoverageState, UsageStoreError> {
    validate_bucket_key(kind, bucket_key)?;
    query_coverage_state_filtered(connection, Some((kind, bucket_key)))
}

fn query_coverage_state_filtered(
    connection: &Connection,
    bucket: Option<(SummaryKind, &str)>,
) -> Result<UsageCoverageState, UsageStoreError> {
    let (start_column, end_column) = match bucket.map(|(kind, _)| kind) {
        Some(SummaryKind::Daily) => ("start_daily_key", "end_daily_key"),
        Some(SummaryKind::Weekly) => ("start_weekly_key", "end_weekly_key"),
        None => ("start_daily_key", "end_daily_key"),
    };
    let where_clause = if bucket.is_some() {
        format!("WHERE {start_column} <= ?1 AND ({end_column} IS NULL OR {end_column} >= ?1)")
    } else {
        String::new()
    };
    let query = format!(
        "SELECT COALESCE(SUM(occurrence_count), 0),
                COALESCE(SUM(recovered_interval_count), 0),
                MAX(COALESCE(ended_wall_utc_ms, started_wall_utc_ms))
         FROM coverage_gaps {where_clause}"
    );
    let (event_gap_count, recovered_interval_count, last_gap_wall_utc_ms): (i64, i64, Option<i64>) =
        match bucket {
            Some((_, bucket_key)) => connection.query_row(&query, [bucket_key], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?,
            None => connection.query_row(&query, [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?,
        };
    let reason_query = format!(
        "SELECT reason FROM coverage_gaps {where_clause}
         ORDER BY COALESCE(ended_wall_utc_ms, started_wall_utc_ms) DESC, id DESC
         LIMIT 1"
    );
    let last_gap_reason = match (last_gap_wall_utc_ms, bucket) {
        (Some(_), Some((_, bucket_key))) => connection
            .query_row(&reason_query, [bucket_key], |row| row.get(0))
            .optional()?,
        (Some(_), None) => connection
            .query_row(&reason_query, [], |row| row.get(0))
            .optional()?,
        (None, _) => None,
    };
    let epoch: Option<(i64, String, String)> = connection
        .query_row(
            "SELECT tracking_started_wall_utc_ms, tracking_start_daily_key,
                    tracking_start_weekly_key
             FROM usage_epoch WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let (tracking_started_wall_utc_ms, tracking_start_daily_key, tracking_start_weekly_key) =
        match epoch {
            Some((started, daily, weekly)) => (Some(started), Some(daily), Some(weekly)),
            None => (None, None, None),
        };
    Ok(UsageCoverageState {
        event_gap_count: u64::try_from(event_gap_count)
            .map_err(|_| UsageStoreError::NumericOverflow)?,
        recovered_interval_count: u64::try_from(recovered_interval_count)
            .map_err(|_| UsageStoreError::NumericOverflow)?,
        last_gap_wall_utc_ms,
        last_gap_reason,
        tracking_started_wall_utc_ms,
        tracking_start_daily_key,
        tracking_start_weekly_key,
    })
}

fn initialize_usage_epoch(
    connection: &Connection,
    recovered_at: &ClockSample,
) -> Result<(), UsageStoreError> {
    let earliest: Option<(i64, String, i32)> = connection
        .query_row(
            "SELECT started_wall_utc_ms, start_timezone_id, start_utc_offset_seconds
             FROM focus_intervals
             ORDER BY started_wall_utc_ms ASC, id ASC
             LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let (wall_utc_ms, timezone_id, utc_offset_seconds) = earliest.unwrap_or_else(|| {
        (
            recovered_at.wall_utc_ms,
            recovered_at.timezone_id.clone(),
            recovered_at.utc_offset_seconds,
        )
    });
    let epoch_sample = ClockSample {
        boot_id: recovered_at.boot_id.clone(),
        monotonic_ns: recovered_at.monotonic_ns,
        wall_utc_ms,
        utc_offset_seconds,
        timezone_id,
    };
    let daily = SummaryBucket::for_sample(SummaryKind::Daily, &epoch_sample)
        .map_err(|_| UsageStoreError::InvalidUsageEpoch)?;
    let weekly = SummaryBucket::for_sample(SummaryKind::Weekly, &epoch_sample)
        .map_err(|_| UsageStoreError::InvalidUsageEpoch)?;
    connection.execute(
        "INSERT OR IGNORE INTO usage_epoch (
            singleton, tracking_started_wall_utc_ms,
            tracking_start_daily_key, tracking_start_weekly_key
         ) VALUES (1, ?1, ?2, ?3)",
        params![wall_utc_ms, daily.bucket_key, weekly.bucket_key],
    )?;
    Ok(())
}

fn migrate_usage_schema(
    connection: &mut Connection,
    recovered_at: &ClockSample,
) -> Result<(), UsageStoreError> {
    let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > USAGE_STORE_SCHEMA_VERSION {
        return Err(UsageStoreError::UnsupportedSchema {
            found: version,
            supported: USAGE_STORE_SCHEMA_VERSION,
        });
    }
    if version > 0 && version < USAGE_STORE_SCHEMA_VERSION {
        ensure_migration_backup(connection, version)?;
    }
    connection.execute_batch("PRAGMA foreign_keys = ON;\nPRAGMA synchronous = FULL;")?;
    if version == 0 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(SCHEMA_V1)?;
        transaction.execute_batch(SCHEMA_V2)?;
        initialize_usage_epoch(&transaction, recovered_at)?;
        transaction.execute("DROP TABLE usage_coverage", [])?;
        transaction.pragma_update(None, "user_version", USAGE_STORE_SCHEMA_VERSION)?;
        transaction.commit()?;
    } else if version == 1 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(SCHEMA_V2)?;
        migrate_legacy_coverage_gap(&transaction, recovered_at)?;
        begin_schema_upgrade_transition(&transaction, recovered_at)?;
        transaction.execute("DROP TABLE usage_coverage", [])?;
        transaction.pragma_update(None, "user_version", USAGE_STORE_SCHEMA_VERSION)?;
        transaction.commit()?;
    }
    connection.execute_batch("PRAGMA journal_mode = WAL;")?;
    Ok(())
}

fn ensure_migration_backup(connection: &Connection, version: u32) -> Result<(), UsageStoreError> {
    let Some(source) = connection.path().filter(|path| !path.is_empty()) else {
        return Ok(());
    };
    let backup = migration_backup_path(Path::new(source), version);
    if backup.exists() {
        return validate_migration_backup(&backup, version);
    }
    let temporary = temporary_backup_path(&backup);
    let result = create_migration_backup(connection, &temporary, &backup, version);
    if temporary.exists() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn migrate_legacy_coverage_gap(
    connection: &Connection,
    recovered_at: &ClockSample,
) -> Result<(), UsageStoreError> {
    let legacy: (i64, i64, Option<i64>, Option<String>) = connection.query_row(
        "SELECT event_gap_count, recovered_interval_count,
                last_gap_wall_utc_ms, last_gap_reason
         FROM usage_coverage WHERE singleton = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let (event_gap_count, recovered_interval_count, Some(wall_utc_ms), reason) = legacy else {
        return Ok(());
    };
    if event_gap_count <= 0 {
        return Ok(());
    }
    let sample = ClockSample {
        wall_utc_ms,
        ..recovered_at.clone()
    };
    let (daily_key, weekly_key) = gap_bucket_keys(&sample)?;
    connection.execute(
        "INSERT INTO coverage_gaps (
            started_wall_utc_ms, ended_wall_utc_ms,
            start_daily_key, end_daily_key, start_weekly_key, end_weekly_key,
            reason, occurrence_count, recovered_interval_count
         ) VALUES (?1, ?1, ?2, ?2, ?3, ?3, ?4, ?5, ?6)",
        params![
            wall_utc_ms,
            daily_key,
            weekly_key,
            reason.unwrap_or_else(|| "legacy_gap".to_owned()),
            event_gap_count,
            recovered_interval_count,
        ],
    )?;
    Ok(())
}

fn begin_schema_upgrade_transition(
    connection: &Connection,
    recovered_at: &ClockSample,
) -> Result<(), UsageStoreError> {
    let latest: Option<(i64, String, i32)> = connection
        .query_row(
            "SELECT last_checkpoint_wall_utc_ms,
                    COALESCE(end_timezone_id, start_timezone_id),
                    COALESCE(end_utc_offset_seconds, start_utc_offset_seconds)
             FROM focus_intervals
             ORDER BY last_checkpoint_wall_utc_ms DESC, id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let start = match latest {
        Some((wall_utc_ms, timezone_id, utc_offset_seconds))
            if wall_utc_ms <= recovered_at.wall_utc_ms =>
        {
            ClockSample {
                wall_utc_ms,
                timezone_id,
                utc_offset_seconds,
                ..recovered_at.clone()
            }
        }
        _ => recovered_at.clone(),
    };
    let (daily_key, weekly_key) = gap_bucket_keys(&start)?;
    connection.execute(
        "INSERT INTO coverage_gaps (
            started_wall_utc_ms, start_daily_key, start_weekly_key, reason
         ) VALUES (?1, ?2, ?3, 'usage_schema_upgrade_transition')",
        params![start.wall_utc_ms, daily_key, weekly_key],
    )?;
    Ok(())
}

fn create_migration_backup(
    connection: &Connection,
    temporary: &Path,
    backup: &Path,
    version: u32,
) -> Result<(), UsageStoreError> {
    let temporary_text = temporary.to_str().ok_or(UsageStoreError::MigrationBackup {
        reason: "usage_migration_backup_path_invalid",
    })?;
    connection
        .execute("VACUUM main INTO ?1", [temporary_text])
        .map_err(|_| UsageStoreError::MigrationBackup {
            reason: "usage_migration_backup_snapshot_failed",
        })?;
    let mut permissions = fs::metadata(temporary)
        .map_err(|_| UsageStoreError::MigrationBackup {
            reason: "usage_migration_backup_metadata_failed",
        })?
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o600);
    }
    fs::set_permissions(temporary, permissions).map_err(|_| UsageStoreError::MigrationBackup {
        reason: "usage_migration_backup_permissions_failed",
    })?;
    validate_migration_backup(temporary, version)?;
    fs::File::open(temporary)
        .and_then(|file| file.sync_all())
        .map_err(|_| UsageStoreError::MigrationBackup {
            reason: "usage_migration_backup_sync_failed",
        })?;
    match fs::hard_link(temporary, backup) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            validate_migration_backup(backup, version)?;
        }
        Err(_) => {
            return Err(UsageStoreError::MigrationBackup {
                reason: "usage_migration_backup_publish_failed",
            });
        }
    }
    sync_parent_directory(backup)
}

fn validate_migration_backup(path: &Path, version: u32) -> Result<(), UsageStoreError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| UsageStoreError::MigrationBackup {
        reason: "usage_migration_backup_metadata_failed",
    })?;
    if !metadata.file_type().is_file() {
        return Err(UsageStoreError::MigrationBackup {
            reason: "usage_migration_backup_unsafe",
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(UsageStoreError::MigrationBackup {
                reason: "usage_migration_backup_unsafe",
            });
        }
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let backup =
        Connection::open_with_flags(path, flags).map_err(|_| UsageStoreError::MigrationBackup {
            reason: "usage_migration_backup_invalid",
        })?;
    let found: u32 = backup
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|_| UsageStoreError::MigrationBackup {
            reason: "usage_migration_backup_invalid",
        })?;
    let check: String = backup
        .pragma_query_value(None, "quick_check", |row| row.get(0))
        .map_err(|_| UsageStoreError::MigrationBackup {
            reason: "usage_migration_backup_invalid",
        })?;
    if found != version || check != "ok" {
        return Err(UsageStoreError::MigrationBackup {
            reason: "usage_migration_backup_invalid",
        });
    }
    Ok(())
}

fn migration_backup_path(source: &Path, version: u32) -> PathBuf {
    let mut name: OsString = source.as_os_str().to_owned();
    name.push(format!(".v{version}.bak"));
    PathBuf::from(name)
}

fn temporary_backup_path(backup: &Path) -> PathBuf {
    let mut name: OsString = backup.as_os_str().to_owned();
    let epoch_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let nonce = BACKUP_NONCE.fetch_add(1, Ordering::Relaxed);
    name.push(format!(".{}.{epoch_nanos}.{nonce}.tmp", process::id()));
    PathBuf::from(name)
}

fn sync_parent_directory(path: &Path) -> Result<(), UsageStoreError> {
    let parent = path.parent().ok_or(UsageStoreError::MigrationBackup {
        reason: "usage_migration_backup_path_invalid",
    })?;
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| UsageStoreError::MigrationBackup {
            reason: "usage_migration_backup_sync_failed",
        })
}

fn ensure_database_integrity(connection: &Connection) -> Result<(), UsageStoreError> {
    let result = connection
        .query_row("PRAGMA quick_check(1)", [], |row| row.get::<_, String>(0))
        .map_err(|_| UsageStoreError::Corrupt)?;
    if result != "ok" {
        return Err(UsageStoreError::Corrupt);
    }
    Ok(())
}

fn ensure_reader_schema(connection: &Connection) -> Result<(), UsageStoreError> {
    let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version != USAGE_STORE_SCHEMA_VERSION {
        return Err(UsageStoreError::UnsupportedSchema {
            found: version,
            supported: USAGE_STORE_SCHEMA_VERSION,
        });
    }
    Ok(())
}

fn validate_stored_app_id(app_id: &str) -> Result<(), UsageStoreError> {
    if app_id.is_empty() || app_id.len() > MAX_NIRI_APP_ID_BYTES || app_id.contains('\0') {
        return Err(UsageStoreError::InvalidAppId);
    }
    Ok(())
}

fn validate_gap_reason(reason: &str) -> Result<(), UsageStoreError> {
    if reason.is_empty() || reason.len() > 128 || reason.contains('\0') {
        return Err(UsageStoreError::InvalidGapReason);
    }
    Ok(())
}

fn gap_bucket_keys(sample: &ClockSample) -> Result<(String, String), UsageStoreError> {
    let daily = SummaryBucket::for_sample(SummaryKind::Daily, sample)
        .map_err(|_| UsageStoreError::InvalidUsageEpoch)?;
    let weekly = SummaryBucket::for_sample(SummaryKind::Weekly, sample)
        .map_err(|_| UsageStoreError::InvalidUsageEpoch)?;
    Ok((daily.bucket_key, weekly.bucket_key))
}

fn upsert_aggregate(
    transaction: &Transaction<'_>,
    table: &str,
    app_id: &str,
    bucket_key: &str,
    slice: &BucketSlice,
    bucket_start_utc_ms: i64,
) -> Result<(), UsageStoreError> {
    let query = format!(
        "INSERT INTO {table} (
            app_id, bucket_key, timezone_id, utc_offset_seconds,
            bucket_start_utc_ms, last_wall_utc_ms, duration_ns
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(app_id, bucket_key, timezone_id, utc_offset_seconds)
         DO UPDATE SET
            duration_ns = duration_ns + excluded.duration_ns,
            last_wall_utc_ms = MAX(last_wall_utc_ms, excluded.last_wall_utc_ms)"
    );
    transaction.execute(
        &query,
        params![
            app_id,
            bucket_key,
            slice.timezone_id,
            slice.utc_offset_seconds,
            bucket_start_utc_ms,
            slice.last_wall_utc_ms,
            sql_u64(slice.duration_ns)?,
        ],
    )?;
    Ok(())
}

fn delete_older_than(
    transaction: &Transaction<'_>,
    table: &str,
    column: &str,
    cutoff: i64,
) -> Result<(), UsageStoreError> {
    let query = format!("DELETE FROM {table} WHERE {column} < ?1");
    transaction.execute(&query, [cutoff])?;
    Ok(())
}

fn cutoff_days(now: i64, days: u32) -> Result<i64, UsageStoreError> {
    let age = i64::from(days)
        .checked_mul(DAY_MS)
        .ok_or(UsageStoreError::NumericOverflow)?;
    now.checked_sub(age).ok_or(UsageStoreError::NumericOverflow)
}

#[derive(Clone, Debug)]
struct WriterIdentity {
    pid: u32,
    start_ticks: u64,
    boot_id: String,
}

impl WriterIdentity {
    fn current() -> Result<Self, UsageStoreError> {
        Ok(Self {
            pid: process::id(),
            start_ticks: process_start_ticks(process::id())?,
            boot_id: read_boot_id()?,
        })
    }
}

fn acquire_writer_lease(
    connection: &mut Connection,
    writer: &WriterIdentity,
    wall_utc_ms: i64,
) -> Result<(), UsageStoreError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing: Option<(u32, i64, String)> = transaction
        .query_row(
            "SELECT owner_pid, owner_start_ticks, owner_boot_id FROM writer_lease WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if let Some((pid, start_ticks, boot_id)) = existing
        && boot_id == writer.boot_id
        && u64::try_from(start_ticks).ok() == process_start_ticks(pid).ok()
    {
        return Err(UsageStoreError::WriterAlreadyActive { pid });
    }
    transaction.execute(
        "INSERT INTO writer_lease (
            singleton, owner_pid, owner_start_ticks, owner_boot_id, acquired_wall_utc_ms
         ) VALUES (1, ?1, ?2, ?3, ?4)
         ON CONFLICT(singleton) DO UPDATE SET
            owner_pid = excluded.owner_pid,
            owner_start_ticks = excluded.owner_start_ticks,
            owner_boot_id = excluded.owner_boot_id,
            acquired_wall_utc_ms = excluded.acquired_wall_utc_ms",
        params![
            i64::from(writer.pid),
            sql_u64(writer.start_ticks)?,
            writer.boot_id,
            wall_utc_ms,
        ],
    )?;
    transaction.commit()?;
    Ok(())
}

fn process_start_ticks(pid: u32) -> Result<u64, UsageStoreError> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let end_of_name = stat.rfind(')').ok_or(UsageStoreError::InvalidProcStat)?;
    let fields: Vec<_> = stat[end_of_name + 1..].split_whitespace().collect();
    fields
        .get(19)
        .ok_or(UsageStoreError::InvalidProcStat)?
        .parse()
        .map_err(|_| UsageStoreError::InvalidProcStat)
}

fn read_boot_id() -> Result<String, UsageStoreError> {
    let value = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    let value = value.trim();
    if value.is_empty() {
        return Err(UsageStoreError::InvalidBootId);
    }
    Ok(value.to_owned())
}

fn sql_u64(value: u64) -> Result<i64, UsageStoreError> {
    i64::try_from(value).map_err(|_| UsageStoreError::NumericOverflow)
}

fn validate_bucket_key(kind: SummaryKind, value: &str) -> Result<(), UsageStoreError> {
    let bytes = value.as_bytes();
    let valid = match kind {
        SummaryKind::Daily => {
            bytes.len() == 10
                && bytes[4] == b'-'
                && bytes[7] == b'-'
                && bytes[..4].iter().all(u8::is_ascii_digit)
                && two_digits_in_range(&bytes[5..7], 1, 12)
                && two_digits_in_range(&bytes[8..10], 1, 31)
        }
        SummaryKind::Weekly => {
            bytes.len() == 8
                && bytes[4] == b'-'
                && bytes[5] == b'W'
                && bytes[..4].iter().all(u8::is_ascii_digit)
                && two_digits_in_range(&bytes[6..8], 1, 53)
        }
    };
    valid.then_some(()).ok_or(UsageStoreError::InvalidBucketKey)
}

fn two_digits_in_range(bytes: &[u8], minimum: u8, maximum: u8) -> bool {
    if bytes.len() != 2 || !bytes.iter().all(u8::is_ascii_digit) {
        return false;
    }
    let value = (bytes[0] - b'0') * 10 + (bytes[1] - b'0');
    (minimum..=maximum).contains(&value)
}

#[derive(Debug, Error)]
pub enum UsageStoreError {
    #[error("SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("system identity read failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("another live usage writer owns the database (pid {pid})")]
    WriterAlreadyActive { pid: u32 },
    #[error("focus interval {0} is not open")]
    IntervalNotOpen(i64),
    #[error("integer cannot be represented safely in SQLite")]
    NumericOverflow,
    #[error("invalid /proc process stat")]
    InvalidProcStat,
    #[error("empty kernel boot id")]
    InvalidBootId,
    #[error("application id is empty, contains NUL, or exceeds 512 bytes")]
    InvalidAppId,
    #[error("usage bucket key is invalid")]
    InvalidBucketKey,
    #[error("usage timezone id is invalid")]
    InvalidTimezoneId,
    #[error("usage tracking epoch is invalid")]
    InvalidUsageEpoch,
    #[error("usage database is corrupt")]
    Corrupt,
    #[error("usage migration backup failed: {reason}")]
    MigrationBackup { reason: &'static str },
    #[error("coverage gap reason is empty, contains NUL, or exceeds 128 bytes")]
    InvalidGapReason,
    #[error("usage schema version {found} is unsupported; expected {supported}")]
    UnsupportedSchema { found: u32, supported: u32 },
}

const SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS writer_lease (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    owner_pid INTEGER NOT NULL,
    owner_start_ticks INTEGER NOT NULL,
    owner_boot_id TEXT NOT NULL,
    acquired_wall_utc_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS focus_intervals (
    id INTEGER PRIMARY KEY,
    app_id TEXT NOT NULL CHECK (length(app_id) > 0),
    pid INTEGER,
    window_id INTEGER NOT NULL,
    boot_id TEXT NOT NULL,
    started_monotonic_ns INTEGER NOT NULL,
    last_checkpoint_monotonic_ns INTEGER NOT NULL,
    ended_monotonic_ns INTEGER,
    started_wall_utc_ms INTEGER NOT NULL,
    last_checkpoint_wall_utc_ms INTEGER NOT NULL,
    ended_wall_utc_ms INTEGER,
    start_timezone_id TEXT NOT NULL,
    start_utc_offset_seconds INTEGER NOT NULL,
    end_timezone_id TEXT,
    end_utc_offset_seconds INTEGER,
    duration_ns INTEGER NOT NULL DEFAULT 0 CHECK (duration_ns >= 0),
    state TEXT NOT NULL CHECK (state IN ('open', 'closed', 'recovered')),
    end_reason TEXT,
    recovered_at_wall_utc_ms INTEGER
) STRICT;
CREATE INDEX IF NOT EXISTS focus_intervals_end_idx
    ON focus_intervals(ended_wall_utc_ms);
CREATE UNIQUE INDEX IF NOT EXISTS one_open_focus_interval
    ON focus_intervals((state)) WHERE state = 'open';

CREATE TABLE IF NOT EXISTS usage_coverage (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    event_gap_count INTEGER NOT NULL DEFAULT 0 CHECK (event_gap_count >= 0),
    recovered_interval_count INTEGER NOT NULL DEFAULT 0 CHECK (recovered_interval_count >= 0),
    last_gap_wall_utc_ms INTEGER,
    last_gap_reason TEXT CHECK (last_gap_reason IS NULL OR length(last_gap_reason) <= 128)
) STRICT;
INSERT OR IGNORE INTO usage_coverage (singleton) VALUES (1);

CREATE TABLE IF NOT EXISTS usage_epoch (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    tracking_started_wall_utc_ms INTEGER NOT NULL,
    tracking_start_daily_key TEXT NOT NULL,
    tracking_start_weekly_key TEXT NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS minute_aggregates (
    app_id TEXT NOT NULL,
    bucket_key TEXT NOT NULL,
    timezone_id TEXT NOT NULL,
    utc_offset_seconds INTEGER NOT NULL,
    bucket_start_utc_ms INTEGER NOT NULL,
    last_wall_utc_ms INTEGER NOT NULL,
    duration_ns INTEGER NOT NULL CHECK (duration_ns >= 0),
    PRIMARY KEY (app_id, bucket_key, timezone_id, utc_offset_seconds)
) STRICT, WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS daily_aggregates (
    app_id TEXT NOT NULL,
    bucket_key TEXT NOT NULL,
    timezone_id TEXT NOT NULL,
    utc_offset_seconds INTEGER NOT NULL,
    bucket_start_utc_ms INTEGER NOT NULL,
    last_wall_utc_ms INTEGER NOT NULL,
    duration_ns INTEGER NOT NULL CHECK (duration_ns >= 0),
    PRIMARY KEY (app_id, bucket_key, timezone_id, utc_offset_seconds)
) STRICT, WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS daily_aggregates_ranking_idx
    ON daily_aggregates(
        bucket_key, duration_ns DESC, app_id, timezone_id,
        utc_offset_seconds, bucket_start_utc_ms
    );
CREATE INDEX IF NOT EXISTS daily_aggregates_segment_ranking_idx
    ON daily_aggregates(
        bucket_key, timezone_id, utc_offset_seconds, bucket_start_utc_ms,
        duration_ns DESC, app_id
    );

CREATE TABLE IF NOT EXISTS weekly_aggregates (
    app_id TEXT NOT NULL,
    bucket_key TEXT NOT NULL,
    timezone_id TEXT NOT NULL,
    utc_offset_seconds INTEGER NOT NULL,
    bucket_start_utc_ms INTEGER NOT NULL,
    last_wall_utc_ms INTEGER NOT NULL,
    duration_ns INTEGER NOT NULL CHECK (duration_ns >= 0),
    PRIMARY KEY (app_id, bucket_key, timezone_id, utc_offset_seconds)
) STRICT, WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS weekly_aggregates_ranking_idx
    ON weekly_aggregates(
        bucket_key, duration_ns DESC, app_id, timezone_id,
        utc_offset_seconds, bucket_start_utc_ms
    );
CREATE INDEX IF NOT EXISTS weekly_aggregates_segment_ranking_idx
    ON weekly_aggregates(
        bucket_key, timezone_id, utc_offset_seconds, bucket_start_utc_ms,
        duration_ns DESC, app_id
    );
"#;

const SCHEMA_V2: &str = r#"
CREATE TABLE IF NOT EXISTS coverage_gaps (
    id INTEGER PRIMARY KEY,
    started_wall_utc_ms INTEGER NOT NULL,
    ended_wall_utc_ms INTEGER,
    start_daily_key TEXT NOT NULL,
    end_daily_key TEXT,
    start_weekly_key TEXT NOT NULL,
    end_weekly_key TEXT,
    reason TEXT NOT NULL CHECK (length(reason) BETWEEN 1 AND 128),
    occurrence_count INTEGER NOT NULL DEFAULT 1 CHECK (occurrence_count > 0),
    recovered_interval_count INTEGER NOT NULL DEFAULT 0 CHECK (recovered_interval_count >= 0),
    CHECK (ended_wall_utc_ms IS NULL OR ended_wall_utc_ms >= started_wall_utc_ms)
) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS one_open_coverage_gap
    ON coverage_gaps((ended_wall_utc_ms IS NULL)) WHERE ended_wall_utc_ms IS NULL;
CREATE INDEX IF NOT EXISTS coverage_gaps_daily_idx
    ON coverage_gaps(start_daily_key, end_daily_key);
CREATE INDEX IF NOT EXISTS coverage_gaps_weekly_idx
    ON coverage_gaps(start_weekly_key, end_weekly_key);

"#;

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn sample(mono: u64, wall: i64) -> ClockSample {
        ClockSample {
            boot_id: "test-boot".into(),
            monotonic_ns: mono,
            wall_utc_ms: wall,
            utc_offset_seconds: 0,
            timezone_id: "UTC".into(),
        }
    }

    #[test]
    fn rejects_a_second_live_writer() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.sqlite3");
        let first = UsageStore::open(&path, &sample(0, 1_000)).unwrap();
        let second = UsageStore::open(&path, &sample(0, 1_000)).unwrap_err();
        assert!(matches!(
            second,
            UsageStoreError::WriterAlreadyActive { .. }
        ));
        drop(first);
        UsageStore::open(&path, &sample(0, 1_000)).unwrap();
    }

    #[test]
    fn crash_recovery_closes_at_last_durable_checkpoint() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.sqlite3");
        let mut store = UsageStore::open(&path, &sample(0, 1_000)).unwrap();
        store
            .start_interval(
                &WindowIdentity {
                    window_id: 9,
                    app_id: "terminal".into(),
                    pid: Some(10),
                },
                &sample(0, 1_000),
            )
            .unwrap();
        // Simulate a dead owner without relying on another process in the unit test.
        store
            .connection
            .execute("DELETE FROM writer_lease", [])
            .unwrap();
        std::mem::forget(store);

        let recovered = UsageStore::open(&path, &sample(10_000, 9_000)).unwrap();
        assert_eq!(recovered.open_interval_count().unwrap(), 0);
        assert_eq!(recovered.interval_count().unwrap(), 1);
        assert_eq!(
            recovered.coverage_state().unwrap(),
            UsageCoverageState {
                event_gap_count: 1,
                recovered_interval_count: 1,
                last_gap_wall_utc_ms: Some(1_000),
                last_gap_reason: Some("crash_recovery".into()),
                tracking_started_wall_utc_ms: Some(1_000),
                tracking_start_daily_key: Some("1970-01-01".into()),
                tracking_start_weekly_key: Some("1970-W01".into()),
            }
        );
    }

    #[test]
    fn gap_interval_only_degrades_intersecting_daily_and_weekly_buckets() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.sqlite3");
        let mut store = UsageStore::open(&path, &sample(0, 1_767_571_190_000)).unwrap();
        let start = sample(0, 1_767_571_190_000); // 2026-01-04 23:59:50 UTC
        let end = sample(20_000_000_000, 1_767_571_210_000); // 2026-01-05 00:00:10 UTC
        store
            .record_gap_interval("event_gap", &start, &end)
            .unwrap();

        for (kind, key) in [
            (SummaryKind::Daily, "2026-01-04"),
            (SummaryKind::Daily, "2026-01-05"),
            (SummaryKind::Weekly, "2026-W01"),
            (SummaryKind::Weekly, "2026-W02"),
        ] {
            assert_eq!(
                store
                    .coverage_state_for_bucket(kind, key)
                    .unwrap()
                    .event_gap_count,
                1,
                "gap missing from {key}"
            );
        }
        for (kind, key) in [
            (SummaryKind::Daily, "2026-01-03"),
            (SummaryKind::Daily, "2026-01-06"),
            (SummaryKind::Weekly, "2026-W03"),
        ] {
            assert_eq!(
                store
                    .coverage_state_for_bucket(kind, key)
                    .unwrap()
                    .event_gap_count,
                0,
                "gap leaked into {key}"
            );
        }
    }

    #[test]
    fn bucket_coverage_reports_the_latest_reason_from_that_bucket() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.sqlite3");
        let mut store = UsageStore::open(&path, &sample(0, 1_767_225_600_000)).unwrap();
        store
            .record_gap_interval(
                "older_bucket_gap",
                &sample(0, 1_767_225_610_000),
                &sample(1, 1_767_225_611_000),
            )
            .unwrap();
        store
            .record_gap_interval(
                "newer_bucket_gap",
                &sample(2, 1_767_312_010_000),
                &sample(3, 1_767_312_011_000),
            )
            .unwrap();

        let first = store
            .coverage_state_for_bucket(SummaryKind::Daily, "2026-01-01")
            .unwrap();
        assert_eq!(first.event_gap_count, 1);
        assert_eq!(first.last_gap_reason.as_deref(), Some("older_bucket_gap"));
        let second = store
            .coverage_state_for_bucket(SummaryKind::Daily, "2026-01-02")
            .unwrap();
        assert_eq!(second.event_gap_count, 1);
        assert_eq!(second.last_gap_reason.as_deref(), Some("newer_bucket_gap"));
    }

    #[test]
    fn open_daemon_gap_survives_reopen_and_closes_in_the_recovery_bucket() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.sqlite3");
        let start = sample(0, 1_767_571_190_000); // 2026-01-04 23:59:50 UTC
        let end = sample(20_000_000_000, 1_767_571_210_000); // 2026-01-05 00:00:10 UTC
        let mut store = UsageStore::open(&path, &start).unwrap();
        store.begin_gap("usage_daemon_stopped", &start).unwrap();
        assert!(store.has_open_gap().unwrap());
        drop(store);

        let reopened = UsageStore::open(&path, &end).unwrap();
        assert!(reopened.has_open_gap().unwrap());
        let mut reopened = reopened;
        reopened.close_open_gaps(&end).unwrap();
        assert!(!reopened.has_open_gap().unwrap());
        for (kind, key) in [
            (SummaryKind::Daily, "2026-01-04"),
            (SummaryKind::Daily, "2026-01-05"),
            (SummaryKind::Weekly, "2026-W01"),
            (SummaryKind::Weekly, "2026-W02"),
        ] {
            assert_eq!(
                reopened
                    .coverage_state_for_bucket(kind, key)
                    .unwrap()
                    .event_gap_count,
                1
            );
        }
        assert_eq!(
            reopened
                .coverage_state_for_bucket(SummaryKind::Daily, "2026-01-06")
                .unwrap()
                .event_gap_count,
            0
        );
    }

    #[test]
    fn version_one_upgrade_is_backed_up_and_legacy_gap_stays_in_its_bucket() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(SCHEMA_V1).unwrap();
        connection
            .execute(
                "INSERT INTO usage_epoch (
                    singleton, tracking_started_wall_utc_ms,
                    tracking_start_daily_key, tracking_start_weekly_key
                 ) VALUES (1, ?1, '2026-01-01', '2026-W01')",
                [1_767_225_600_000_i64],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE usage_coverage
                 SET event_gap_count = 2, recovered_interval_count = 1,
                     last_gap_wall_utc_ms = ?1, last_gap_reason = 'legacy_disconnect'
                 WHERE singleton = 1",
                [1_767_484_800_000_i64], // 2026-01-04 UTC
            )
            .unwrap();
        connection
            .pragma_update(None, "user_version", 1_u32)
            .unwrap();
        drop(connection);

        let store = UsageStore::open(&path, &sample(0, 1_767_571_200_000)).unwrap();
        assert_eq!(
            store
                .coverage_state_for_bucket(SummaryKind::Daily, "2026-01-04")
                .unwrap()
                .event_gap_count,
            2
        );
        assert_eq!(
            store
                .coverage_state_for_bucket(SummaryKind::Daily, "2026-01-05")
                .unwrap()
                .event_gap_count,
            1
        );
        let mut store = store;
        store
            .close_open_gaps(&sample(0, 1_767_571_200_000))
            .unwrap();
        assert_eq!(
            store
                .coverage_state_for_bucket(SummaryKind::Daily, "2026-01-06")
                .unwrap()
                .event_gap_count,
            0
        );
        assert!(matches!(
            store.connection.query_row::<i64, _, _>(
                "SELECT COUNT(*) FROM usage_coverage",
                [],
                |row| row.get(0)
            ),
            Err(rusqlite::Error::SqliteFailure(_, _))
        ));

        let backup = migration_backup_path(&path, 1);
        let backup_connection = Connection::open_with_flags(
            backup,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .unwrap();
        assert_eq!(
            backup_connection
                .pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))
                .unwrap(),
            1
        );
        assert_eq!(
            backup_connection
                .pragma_query_value::<String, _>(None, "quick_check", |row| row.get(0))
                .unwrap(),
            "ok"
        );
    }

    #[test]
    fn invalid_existing_migration_backup_blocks_upgrade() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(SCHEMA_V1).unwrap();
        initialize_usage_epoch(&connection, &sample(0, 1_767_225_600_000)).unwrap();
        connection
            .pragma_update(None, "user_version", 1_u32)
            .unwrap();
        drop(connection);
        let backup = migration_backup_path(&path, 1);
        fs::write(&backup, b"invalid backup").unwrap();
        #[cfg(unix)]
        fs::set_permissions(&backup, fs::Permissions::from_mode(0o600)).unwrap();

        assert!(matches!(
            UsageStore::open(&path, &sample(0, 1_767_225_600_000)),
            Err(UsageStoreError::MigrationBackup {
                reason: "usage_migration_backup_invalid"
            })
        ));
        let connection = Connection::open(&path).unwrap();
        assert_eq!(
            connection
                .pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))
                .unwrap(),
            1
        );
        assert_eq!(fs::read(backup).unwrap(), b"invalid backup");
    }

    #[test]
    fn retention_removes_closed_history_but_never_the_open_interval() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.sqlite3");
        let mut store = UsageStore::open(&path, &sample(0, 0)).unwrap();
        let identity = WindowIdentity {
            window_id: 9,
            app_id: "terminal".into(),
            pid: Some(10),
        };
        let old = store.start_interval(&identity, &sample(0, 0)).unwrap();
        store.close_interval(old, &sample(0, 0), "test").unwrap();
        store.start_interval(&identity, &sample(1, 1_000)).unwrap();
        store
            .apply_retention(
                2 * DAY_MS,
                RetentionPolicy {
                    raw_days: 1,
                    ..RetentionPolicy::default()
                },
            )
            .unwrap();
        assert_eq!(store.interval_count().unwrap(), 1);
        assert_eq!(store.open_interval_count().unwrap(), 1);
    }

    #[test]
    fn retention_preserves_gaps_for_a_retained_boundary_week() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.sqlite3");
        let now = 1_768_392_000_000_i64; // 2026-01-14 12:00:00 UTC
        let mut store = UsageStore::open(&path, &sample(0, now)).unwrap();
        let retained_gap = 1_767_571_200_000_i64; // 2026-01-05 00:00:00 UTC
        let retained_last = 1_768_175_999_000_i64; // 2026-01-11 23:59:59 UTC
        let expired_gap = 1_766_966_400_000_i64; // 2025-12-29 00:00:00 UTC
        let expired_last = 1_767_571_199_000_i64; // 2026-01-04 23:59:59 UTC
        store
            .record_gap_interval(
                "retained_boundary_week",
                &sample(0, retained_gap),
                &sample(1, retained_gap + 1_000),
            )
            .unwrap();
        store
            .record_gap_interval(
                "expired_week",
                &sample(2, expired_gap),
                &sample(3, expired_gap + 1_000),
            )
            .unwrap();
        store
            .connection
            .execute(
                "INSERT INTO weekly_aggregates (
                    app_id, bucket_key, timezone_id, utc_offset_seconds,
                    bucket_start_utc_ms, last_wall_utc_ms, duration_ns
                 ) VALUES ('retained', '2026-W02', 'UTC', 0, ?1, ?2, 1),
                          ('expired', '2026-W01', 'UTC', 0, ?3, ?4, 1)",
                params![retained_gap, retained_last, expired_gap, expired_last],
            )
            .unwrap();

        store
            .apply_retention(
                now,
                RetentionPolicy {
                    raw_days: 1,
                    minute_days: 1,
                    daily_days: 1,
                    weekly_weeks: 1,
                },
            )
            .unwrap();

        assert_eq!(
            store
                .coverage_state_for_bucket(SummaryKind::Weekly, "2026-W02")
                .unwrap()
                .last_gap_reason
                .as_deref(),
            Some("retained_boundary_week")
        );
        assert_eq!(
            store
                .coverage_state_for_bucket(SummaryKind::Weekly, "2026-W01")
                .unwrap()
                .event_gap_count,
            0
        );
        assert_eq!(
            store
                .aggregate_duration_ns(AggregateKind::Weekly, "retained", "2026-W02")
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .aggregate_duration_ns(AggregateKind::Weekly, "expired", "2026-W01")
                .unwrap(),
            0
        );
    }

    #[test]
    fn summary_query_is_segmented_sorted_and_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.sqlite3");
        let mut store = UsageStore::open(&path, &sample(0, 1_767_225_600_000)).unwrap();
        let bucket = SummaryBucket::for_sample(
            SummaryKind::Daily,
            &ClockSample {
                boot_id: "test-boot".into(),
                monotonic_ns: 0,
                wall_utc_ms: 1_767_225_600_000,
                utc_offset_seconds: 0,
                timezone_id: "UTC".into(),
            },
        )
        .unwrap();
        store
            .connection
            .execute(
                "INSERT INTO daily_aggregates
                 (app_id, bucket_key, timezone_id, utc_offset_seconds,
                  bucket_start_utc_ms, last_wall_utc_ms, duration_ns)
                 VALUES ('zeta', ?1, 'UTC', 0, ?2, ?2, 5),
                        ('alpha', ?1, 'UTC', 0, ?2, ?2, 5),
                        ('other-zone', ?1, 'Etc/UTC', 0, ?2, ?2, 99)",
                params![bucket.bucket_key, bucket.bucket_start_utc_ms],
            )
            .unwrap();
        let result = store.aggregates_for_bucket(&bucket).unwrap();
        assert_eq!(
            result.entries,
            vec![
                AggregateEntry {
                    app_id: "alpha".into(),
                    duration_ns: 5,
                },
                AggregateEntry {
                    app_id: "zeta".into(),
                    duration_ns: 5,
                },
            ]
        );
        assert!(!result.truncated);

        let weekly = SummaryBucket::for_sample(
            SummaryKind::Weekly,
            &ClockSample {
                boot_id: "test-boot".into(),
                monotonic_ns: 0,
                wall_utc_ms: 1_767_225_600_000,
                utc_offset_seconds: 0,
                timezone_id: "UTC".into(),
            },
        )
        .unwrap();
        let transaction = store.connection.transaction().unwrap();
        {
            let mut insert = transaction
                .prepare(
                    "INSERT INTO weekly_aggregates
                     (app_id, bucket_key, timezone_id, utc_offset_seconds,
                      bucket_start_utc_ms, last_wall_utc_ms, duration_ns)
                     VALUES (?1, ?2, 'UTC', 0, ?3, ?3, ?4)",
                )
                .unwrap();
            for index in 0..=MAX_SUMMARY_APPLICATIONS {
                insert
                    .execute(params![
                        format!("app-{index:04}"),
                        weekly.bucket_key,
                        weekly.bucket_start_utc_ms,
                        i64::try_from(index + 1).unwrap(),
                    ])
                    .unwrap();
            }
        }
        transaction.commit().unwrap();
        let bounded = store.aggregates_for_bucket(&weekly).unwrap();
        assert_eq!(bounded.entries.len(), MAX_SUMMARY_APPLICATIONS);
        assert!(bounded.truncated);
        assert_eq!(bounded.entries[0].app_id, "app-1024");
    }

    #[test]
    fn key_query_preserves_all_timezone_segments_and_rejects_untyped_keys() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.sqlite3");
        let store = UsageStore::open(&path, &sample(0, 1_767_225_600_000)).unwrap();
        store
            .connection
            .execute(
                "INSERT INTO daily_aggregates
                 (app_id, bucket_key, timezone_id, utc_offset_seconds,
                  bucket_start_utc_ms, last_wall_utc_ms, duration_ns)
                 VALUES ('editor', '2026-01-01', 'UTC', 0, 1767225600000, 1767225601000, 7),
                        ('editor', '2026-01-01', 'Asia/Shanghai', 28800,
                         1767196800000, 1767225602000, 9),
                        ('browser', '2026-01-01', 'UTC', 0,
                         1767225600000, 1767225603000, 9)",
                [],
            )
            .unwrap();

        let result = store
            .aggregate_segments_for_key(SummaryKind::Daily, "2026-01-01")
            .unwrap();
        assert!(!result.truncated);
        assert_eq!(result.entries[0].app_id, "browser");
        assert_eq!(result.entries[1].timezone_id, "Asia/Shanghai");
        assert_eq!(result.entries[2].timezone_id, "UTC");
        assert_eq!(
            store
                .aggregate_segments_for_key(SummaryKind::Daily, "2026-W01")
                .unwrap_err()
                .to_string(),
            "usage bucket key is invalid"
        );

        let reader = UsageReader::open(&path).unwrap();
        assert_eq!(
            reader
                .aggregate_segments_for_key(SummaryKind::Daily, "2026-01-01")
                .unwrap()
                .entries,
            result.entries
        );
        assert_eq!(
            reader.coverage_state().unwrap(),
            store.coverage_state().unwrap()
        );
    }

    #[test]
    fn epoch_is_stable_and_backfills_from_the_first_existing_interval() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.sqlite3");
        let first_sample = sample(0, 1_767_326_400_000);
        let store = UsageStore::open(&path, &first_sample).unwrap();
        let version: u32 = store
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, USAGE_STORE_SCHEMA_VERSION);
        assert_eq!(
            store.coverage_state().unwrap().tracking_started_wall_utc_ms,
            Some(first_sample.wall_utc_ms)
        );
        drop(store);

        let later_sample = sample(0, first_sample.wall_utc_ms + DAY_MS);
        let reopened = UsageStore::open(&path, &later_sample).unwrap();
        assert_eq!(
            reopened
                .coverage_state()
                .unwrap()
                .tracking_started_wall_utc_ms,
            Some(first_sample.wall_utc_ms)
        );
        drop(reopened);

        let legacy_path = directory.path().join("legacy.sqlite3");
        let connection = Connection::open(&legacy_path).unwrap();
        connection.execute_batch(SCHEMA_V1).unwrap();
        connection
            .execute(
                "INSERT INTO focus_intervals (
                    app_id, window_id, boot_id, started_monotonic_ns,
                    last_checkpoint_monotonic_ns, started_wall_utc_ms,
                    last_checkpoint_wall_utc_ms, start_timezone_id,
                    start_utc_offset_seconds, duration_ns, state
                 ) VALUES ('editor', 1, 'test-boot', 0, 0, ?1, ?1,
                           'Asia/Shanghai', 28800, 0, 'closed')",
                [1_786_503_864_075_i64],
            )
            .unwrap();
        drop(connection);

        let backfilled = UsageStore::open(&legacy_path, &later_sample).unwrap();
        let version: u32 = backfilled
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, USAGE_STORE_SCHEMA_VERSION);
        let coverage = backfilled.coverage_state().unwrap();
        assert_eq!(
            coverage.tracking_started_wall_utc_ms,
            Some(1_786_503_864_075)
        );
        assert_eq!(
            coverage.tracking_start_daily_key.as_deref(),
            Some("2026-08-12")
        );
        assert_eq!(
            coverage.tracking_start_weekly_key.as_deref(),
            Some("2026-W33")
        );
    }

    #[test]
    fn future_schema_is_rejected_without_modifying_the_database() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("future.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE future_marker (value TEXT NOT NULL) STRICT;
                 INSERT INTO future_marker VALUES ('preserve-me');
                 PRAGMA user_version = 99;",
            )
            .unwrap();
        drop(connection);
        let before = fs::read(&path).unwrap();

        let error = UsageStore::open(&path, &sample(0, 1_000)).unwrap_err();
        assert!(matches!(
            error,
            UsageStoreError::UnsupportedSchema {
                found: 99,
                supported: USAGE_STORE_SCHEMA_VERSION,
            }
        ));
        assert_eq!(fs::read(&path).unwrap(), before);

        let connection = Connection::open(&path).unwrap();
        let marker: String = connection
            .query_row("SELECT value FROM future_marker", [], |row| row.get(0))
            .unwrap();
        let objects: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name LIKE 'usage_%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(marker, "preserve-me");
        assert_eq!(objects, 0);
        let reader_error = match UsageReader::open(&path) {
            Ok(_) => panic!("future schema reader must be rejected"),
            Err(error) => error,
        };
        assert!(matches!(
            reader_error,
            UsageStoreError::UnsupportedSchema { found: 99, .. }
        ));
    }

    #[test]
    fn corrupt_database_is_rejected_without_rebuild_or_sidecars() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("corrupt.sqlite3");
        let before = b"not a sqlite database";
        fs::write(&path, before).unwrap();

        assert!(matches!(
            UsageStore::open(&path, &sample(0, 1_000)),
            Err(UsageStoreError::Corrupt)
        ));
        assert!(matches!(
            UsageReader::open(&path),
            Err(UsageStoreError::Corrupt)
        ));
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(!path.with_extension("sqlite3-wal").exists());
        assert!(!path.with_extension("sqlite3-shm").exists());
    }
}
