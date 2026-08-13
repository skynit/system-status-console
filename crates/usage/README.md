# localdesk-usage

`localdesk-usage` records foreground application time on niri.
It is a library crate: the appd owner must wire its event loop, polling cadence,
database location, and capability reporting.

## Facts and boundaries

- Focus comes only from `niri msg --json event-stream`. The parser consumes
  `WindowsChanged`, `WindowOpenedOrChanged`, `WindowClosed`, and
  `WindowFocusChanged`; window presence and process uptime never imply focus.
  Polling is cancellable at a caller-supplied deadline; one JSON line is capped
  at 4 MiB, state at 4096 windows, and each `app_id` at 512 bytes.
- Input-idle facts come directly from niri's `ext-idle-notify-v1` version 2
  `get_input_idle_notification` with a 300-second timeout. The first 300 seconds
  after input remain attributed to the focused application; an `idled` edge
  pauses accounting and a `resumed` edge starts it again. A missing or
  disconnected idle stream stops accounting instead of guessing.
- Active and lock facts come from systemd-logind via `loginctl show-session`:
  `Active` and `LockedHint`. `IdleHint` is not trusted because this target does
  not update it. A failed or incomplete probe stops accounting. A persistent,
  object-path-filtered `gdbus monitor` of logind `PropertiesChanged` supplies
  authoritative `Active` and `LockedHint` edge
  notifications; those edges close accounting at the received monotonic edge
  before a fresh property probe.
  Changes to unrelated session properties do not split or pause usage intervals.
  `org.freedesktop.ScreenSaver.GetActive` is not used because the target desktop
  did not expose that method.
  `loginctl` is killed and reaped after two seconds; combined stdout/stderr is
  capped at 64 KiB and the session identifier at 128 bytes.
- Durations use Linux `CLOCK_MONOTONIC`, which does not advance during suspend.
  Local wall time is used only to choose minute, local-day, and local ISO-week
  buckets. Wall-clock jumps, timezone changes, boot changes, and stale event
  gaps are discontinuities; unknown time is dropped rather than guessed.
- A focus interval is open only while focus is known, the session is active and
  unlocked, and the compositor has not reported 300 seconds without input.
  Missing `app_id` is not attributed.
- `UsageStore` is the sole mutable SQLite owner. A boot-id/PID/process-start
  lease rejects a second live writer. Interval checkpoints and all aggregates
  advance in one transaction. On restart, open rows close at their last durable
  checkpoint with `crash_recovery`, so downtime is never counted.
- `UsageReader` opens a separate read-only/query-only connection. Summary scans
  use bucket-leading ranking indexes and `LIMIT 1025`; callers can interrupt a
  timed-out read without blocking or interrupting the sole writer.

## SQLite model

- `focus_intervals`: raw app_id/PID/window focus evidence and monotonic duration.
- `minute_aggregates`: local minute plus UTC offset and timezone identity.
- `daily_aggregates`: local calendar date, timezone/offset segment, and that
  segment's local-midnight UTC start.
- `weekly_aggregates`: local ISO week, timezone/offset segment, and that
  segment's local-Monday UTC start.
- `writer_lease`: exactly one live mutation owner.
- `usage_coverage`: cumulative gap/recovery count and the latest gap wall time
  and reason, allowing restart-time coverage decisions without backfilling.

`SummaryBucket` is the typed daily/weekly query key. Its query returns at most
1024 applications ordered by `duration_ns DESC, app_id ASC`; a larger result is
reported with `truncated` rather than implied complete; queries fetch at most 1025 rows.

`RetentionPolicy` independently expires raw intervals, minute rows, daily rows,
and weekly rows. Aggregate retention uses each row's last contributing wall
timestamp, avoiding reinterpretation after a timezone change.

## Upstream selection record (2026-08-08)

- niri 26.04 commit `8ed0da4`, GPL-3.0-or-later, active upstream with tests and
  direct Linux/Wayland runtime evidence: <https://github.com/niri-wm/niri>.
  This crate uses its stable JSON command boundary and does not copy or link its
  GPL implementation.
- rusqlite 0.40.1, MIT, active and tested: <https://github.com/rusqlite/rusqlite>.
  Default features are disabled and the target's system SQLite is linked.
- chrono 0.4.45, MIT OR Apache-2.0, active and tested:
  <https://github.com/chronotope/chrono>.
- serde 1.0.228 / serde_json 1.0.150, MIT OR Apache-2.0, active and tested:
  <https://github.com/serde-rs/serde>.
- wayland-client 0.31.15 and wayland-protocols 0.32.13, MIT, active and tested:
  <https://github.com/Smithay/wayland-rs>. Only the standardized
  `ext-idle-notify-v1` client protocol is used.
The target inspected for this crate had niri 26.04, systemd 261, SQLite 3.53.4,
and a Wayland logind session.
