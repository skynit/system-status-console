use chrono::{DateTime, Datelike, Duration, FixedOffset, Offset, TimeZone, Timelike, Utc};
use thiserror::Error;

use crate::ClockSample;

const MINUTE_MS: i64 = 60_000;
const MAX_WALL_DRIFT_NS: i128 = 5_000_000_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BucketSlice {
    pub minute_key: String,
    pub day_key: String,
    pub week_key: String,
    pub timezone_id: String,
    pub utc_offset_seconds: i32,
    pub bucket_start_utc_ms: i64,
    pub day_start_utc_ms: i64,
    pub week_start_utc_ms: i64,
    pub last_wall_utc_ms: i64,
    pub duration_ns: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SummaryKind {
    Daily,
    Weekly,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SummaryBucket {
    pub kind: SummaryKind,
    pub bucket_key: String,
    pub timezone_id: String,
    pub utc_offset_seconds: i32,
    pub bucket_start_utc_ms: i64,
}

impl SummaryBucket {
    pub fn for_sample(kind: SummaryKind, sample: &ClockSample) -> Result<Self, BucketError> {
        let local = local_datetime(sample.wall_utc_ms, sample.utc_offset_seconds)?;
        let (day_key, week_key, day_start_utc_ms, week_start_utc_ms) =
            local_summary_identity(local)?;
        Ok(Self {
            kind,
            bucket_key: match kind {
                SummaryKind::Daily => day_key,
                SummaryKind::Weekly => week_key,
            },
            timezone_id: sample.timezone_id.clone(),
            utc_offset_seconds: sample.utc_offset_seconds,
            bucket_start_utc_ms: match kind {
                SummaryKind::Daily => day_start_utc_ms,
                SummaryKind::Weekly => week_start_utc_ms,
            },
        })
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum BucketError {
    #[error("boot changed")]
    BootChanged,
    #[error("monotonic clock did not advance")]
    MonotonicRegression,
    #[error("timezone changed")]
    TimezoneChanged,
    #[error("wall clock did not advance")]
    WallClockRegression,
    #[error("wall and monotonic clocks diverged")]
    WallClockJump,
    #[error("wall timestamp is out of range")]
    InvalidWallTime,
}

pub fn split_into_local_minutes(
    start: &ClockSample,
    end: &ClockSample,
) -> Result<Vec<BucketSlice>, BucketError> {
    if start.boot_id != end.boot_id {
        return Err(BucketError::BootChanged);
    }
    if end.monotonic_ns <= start.monotonic_ns {
        return Err(BucketError::MonotonicRegression);
    }
    if start.utc_offset_seconds != end.utc_offset_seconds || start.timezone_id != end.timezone_id {
        return Err(BucketError::TimezoneChanged);
    }
    let wall_ms = end
        .wall_utc_ms
        .checked_sub(start.wall_utc_ms)
        .ok_or(BucketError::WallClockRegression)?;
    if wall_ms <= 0 {
        return Err(BucketError::WallClockRegression);
    }
    let monotonic_ns = end.monotonic_ns - start.monotonic_ns;
    let wall_ns = i128::from(wall_ms) * 1_000_000;
    if (wall_ns - i128::from(monotonic_ns)).abs() > MAX_WALL_DRIFT_NS {
        return Err(BucketError::WallClockJump);
    }

    let offset_ms = i64::from(start.utc_offset_seconds) * 1_000;
    let local_start = start
        .wall_utc_ms
        .checked_add(offset_ms)
        .ok_or(BucketError::InvalidWallTime)?;
    let local_end = end
        .wall_utc_ms
        .checked_add(offset_ms)
        .ok_or(BucketError::InvalidWallTime)?;
    let mut cursor_local = local_start;
    let mut assigned_ns = 0_u64;
    let mut slices = Vec::new();

    while cursor_local < local_end {
        let next_minute = cursor_local
            .div_euclid(MINUTE_MS)
            .checked_add(1)
            .and_then(|minute| minute.checked_mul(MINUTE_MS))
            .ok_or(BucketError::InvalidWallTime)?;
        let slice_end = next_minute.min(local_end);
        let elapsed_wall =
            u128::try_from(slice_end - local_start).map_err(|_| BucketError::InvalidWallTime)?;
        let total_wall =
            u128::try_from(local_end - local_start).map_err(|_| BucketError::InvalidWallTime)?;
        let cumulative_ns = if slice_end == local_end {
            monotonic_ns
        } else {
            u64::try_from(u128::from(monotonic_ns) * elapsed_wall / total_wall)
                .map_err(|_| BucketError::InvalidWallTime)?
        };
        let duration_ns = cumulative_ns - assigned_ns;
        if duration_ns > 0 {
            slices.push(bucket_for_local_millis(
                cursor_local,
                slice_end,
                start,
                duration_ns,
            )?);
        }
        assigned_ns = cumulative_ns;
        cursor_local = slice_end;
    }
    Ok(slices)
}

fn bucket_for_local_millis(
    local_ms: i64,
    slice_end_local_ms: i64,
    sample: &ClockSample,
    duration_ns: u64,
) -> Result<BucketSlice, BucketError> {
    let offset =
        FixedOffset::east_opt(sample.utc_offset_seconds).ok_or(BucketError::InvalidWallTime)?;
    let utc_ms = local_ms
        .checked_sub(i64::from(sample.utc_offset_seconds) * 1_000)
        .ok_or(BucketError::InvalidWallTime)?;
    let local = local_datetime(utc_ms, sample.utc_offset_seconds)?;
    let minute = local
        .with_second(0)
        .and_then(|value| value.with_nanosecond(0))
        .ok_or(BucketError::InvalidWallTime)?;
    let (day_key, week_key, day_start_utc_ms, week_start_utc_ms) = local_summary_identity(local)?;
    Ok(BucketSlice {
        minute_key: minute.format("%Y-%m-%dT%H:%M%:z").to_string(),
        day_key,
        week_key,
        timezone_id: sample.timezone_id.clone(),
        utc_offset_seconds: offset.fix().local_minus_utc(),
        bucket_start_utc_ms: minute.timestamp_millis(),
        day_start_utc_ms,
        week_start_utc_ms,
        last_wall_utc_ms: slice_end_local_ms - i64::from(sample.utc_offset_seconds) * 1_000,
        duration_ns,
    })
}

fn local_datetime(
    wall_utc_ms: i64,
    utc_offset_seconds: i32,
) -> Result<DateTime<FixedOffset>, BucketError> {
    let offset = FixedOffset::east_opt(utc_offset_seconds).ok_or(BucketError::InvalidWallTime)?;
    Ok(DateTime::<Utc>::from_timestamp_millis(wall_utc_ms)
        .ok_or(BucketError::InvalidWallTime)?
        .with_timezone(&offset))
}

fn local_summary_identity(
    local: DateTime<FixedOffset>,
) -> Result<(String, String, i64, i64), BucketError> {
    let offset = *local.offset();
    let day = local.date_naive();
    let day_start = offset
        .from_local_datetime(
            &day.and_hms_opt(0, 0, 0)
                .ok_or(BucketError::InvalidWallTime)?,
        )
        .single()
        .ok_or(BucketError::InvalidWallTime)?;
    let week_day = day
        .checked_sub_signed(Duration::days(i64::from(
            local.weekday().num_days_from_monday(),
        )))
        .ok_or(BucketError::InvalidWallTime)?;
    let week_start = offset
        .from_local_datetime(
            &week_day
                .and_hms_opt(0, 0, 0)
                .ok_or(BucketError::InvalidWallTime)?,
        )
        .single()
        .ok_or(BucketError::InvalidWallTime)?;
    let iso = local.iso_week();
    Ok((
        local.format("%Y-%m-%d").to_string(),
        format!("{:04}-W{:02}", iso.year(), iso.week()),
        day_start.timestamp_millis(),
        week_start.timestamp_millis(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(mono: u64, wall: i64, offset: i32, timezone: &str) -> ClockSample {
        ClockSample {
            boot_id: "boot-a".into(),
            monotonic_ns: mono,
            wall_utc_ms: wall,
            utc_offset_seconds: offset,
            timezone_id: timezone.into(),
        }
    }

    #[test]
    fn splits_across_local_midnight_and_iso_week() {
        let start = sample(0, 1_767_225_599_500, 0, "UTC"); // 2025-12-31 23:59:59.5
        let end = sample(2_000_000_000, 1_767_225_601_500, 0, "UTC");
        let slices = split_into_local_minutes(&start, &end).unwrap();
        assert_eq!(slices.len(), 2);
        assert_eq!(slices[0].day_key, "2025-12-31");
        assert_eq!(slices[1].day_key, "2026-01-01");
        assert_eq!(
            slices.iter().map(|slice| slice.duration_ns).sum::<u64>(),
            2_000_000_000
        );
    }

    #[test]
    fn rejects_timezone_and_wall_clock_changes() {
        let start = sample(10, 10_000, 0, "UTC");
        let changed_zone = sample(1_000_000_010, 11_000, 3600, "Europe/Paris");
        assert_eq!(
            split_into_local_minutes(&start, &changed_zone),
            Err(BucketError::TimezoneChanged)
        );
        let jumped = sample(1_000_000_010, 30_000, 0, "UTC");
        assert_eq!(
            split_into_local_minutes(&start, &jumped),
            Err(BucketError::WallClockJump)
        );
    }

    #[test]
    fn daily_and_weekly_starts_are_local_period_starts() {
        let value = sample(0, 1_767_326_400_000, 8 * 3_600, "Asia/Shanghai");
        let daily = SummaryBucket::for_sample(SummaryKind::Daily, &value).unwrap();
        let weekly = SummaryBucket::for_sample(SummaryKind::Weekly, &value).unwrap();
        assert_eq!(daily.bucket_key, "2026-01-02");
        assert_eq!(daily.bucket_start_utc_ms, 1_767_283_200_000);
        assert_eq!(weekly.bucket_key, "2026-W01");
        assert_eq!(weekly.bucket_start_utc_ms, 1_766_937_600_000);
    }
}
