use std::fs;
use std::io;
use std::path::Path;

use chrono::{Local, Offset};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClockSample {
    pub boot_id: String,
    pub monotonic_ns: u64,
    pub wall_utc_ms: i64,
    pub utc_offset_seconds: i32,
    pub timezone_id: String,
}

impl ClockSample {
    pub fn interpolate_monotonic(&self, end: &Self, target_ns: u64) -> Option<Self> {
        let mono_span = end.monotonic_ns.checked_sub(self.monotonic_ns)?;
        if target_ns < self.monotonic_ns || target_ns > end.monotonic_ns || mono_span == 0 {
            return None;
        }
        let wall_span = end.wall_utc_ms.checked_sub(self.wall_utc_ms)?;
        let elapsed = target_ns - self.monotonic_ns;
        let wall_delta = i128::from(wall_span) * i128::from(elapsed) / i128::from(mono_span);
        let wall_utc_ms = i128::from(self.wall_utc_ms) + wall_delta;
        Some(Self {
            boot_id: self.boot_id.clone(),
            monotonic_ns: target_ns,
            wall_utc_ms: i64::try_from(wall_utc_ms).ok()?,
            utc_offset_seconds: self.utc_offset_seconds,
            timezone_id: self.timezone_id.clone(),
        })
    }
}

pub trait ClockSource {
    fn sample(&mut self) -> io::Result<ClockSample>;
}

#[derive(Debug, Default)]
pub struct SystemClock;

impl ClockSource for SystemClock {
    fn sample(&mut self) -> io::Result<ClockSample> {
        let local = Local::now();
        Ok(ClockSample {
            boot_id: read_trimmed("/proc/sys/kernel/random/boot_id")?,
            monotonic_ns: monotonic_ns()?,
            wall_utc_ms: local.timestamp_millis(),
            utc_offset_seconds: local.offset().fix().local_minus_utc(),
            timezone_id: local_timezone_id(),
        })
    }
}

fn monotonic_ns() -> io::Result<u64> {
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `value` is a valid writable timespec and CLOCK_MONOTONIC needs no
    // additional lifetime or ownership guarantees.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut value) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let seconds = u64::try_from(value.tv_sec)
        .map_err(|_| io::Error::other("CLOCK_MONOTONIC returned a negative value"))?;
    let nanos = u64::try_from(value.tv_nsec)
        .map_err(|_| io::Error::other("CLOCK_MONOTONIC returned invalid nanoseconds"))?;
    seconds
        .checked_mul(1_000_000_000)
        .and_then(|base| base.checked_add(nanos))
        .ok_or_else(|| io::Error::other("CLOCK_MONOTONIC overflow"))
}

fn local_timezone_id() -> String {
    if let Some(value) = std::env::var_os("TZ").filter(|value| !value.is_empty()) {
        return value.to_string_lossy().into_owned();
    }
    fs::read_link("/etc/localtime")
        .ok()
        .and_then(|path| {
            path.to_str()
                .and_then(|value| value.split("/zoneinfo/").nth(1))
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "local".to_owned())
}

fn read_trimmed(path: impl AsRef<Path>) -> io::Result<String> {
    let value = fs::read_to_string(path)?;
    let value = value.trim();
    if value.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "empty value"));
    }
    Ok(value.to_owned())
}
