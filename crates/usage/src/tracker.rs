use thiserror::Error;

use crate::{
    BucketError, ClockSample, NiriUpdate, SessionSnapshot, UsageStore, UsageStoreError,
    WindowIdentity, split_into_local_minutes,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackerConfig {
    /// Maximum time between durable samples before the unsampled segment is dropped.
    pub max_event_gap_ns: u64,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self {
            max_event_gap_ns: 30_000_000_000,
        }
    }
}

#[derive(Debug)]
pub struct UsageTracker {
    store: UsageStore,
    config: TrackerConfig,
    source_connected: bool,
    focus: Option<WindowIdentity>,
    session: Option<SessionSnapshot>,
    last_sample: Option<ClockSample>,
    active: Option<ActiveInterval>,
    gap_open: bool,
}

#[derive(Clone, Debug)]
struct ActiveInterval {
    id: i64,
    identity: WindowIdentity,
    last_counted: ClockSample,
}

impl UsageTracker {
    pub fn new(store: UsageStore, config: TrackerConfig) -> Self {
        let gap_open = store.has_open_gap().unwrap_or(true);
        Self {
            store,
            config,
            source_connected: false,
            focus: None,
            session: None,
            last_sample: None,
            active: None,
            gap_open,
        }
    }

    pub fn observe_niri_update(
        &mut self,
        update: NiriUpdate,
        session: SessionSnapshot,
        now: ClockSample,
    ) -> Result<(), TrackerError> {
        match update {
            NiriUpdate::FocusChanged(focus) => self.observe_focus(focus, session, now),
            NiriUpdate::StateChanged | NiriUpdate::Ignored => self.checkpoint(session, now),
        }
    }

    pub fn observe_focus(
        &mut self,
        focus: Option<WindowIdentity>,
        session: SessionSnapshot,
        now: ClockSample,
    ) -> Result<(), TrackerError> {
        self.resume_or_advance(&now, &session, true)?;
        self.source_connected = true;
        self.focus = focus;
        self.session = Some(session);
        self.reconcile(&now, "focus_or_session_changed")
    }

    pub fn checkpoint(
        &mut self,
        session: SessionSnapshot,
        now: ClockSample,
    ) -> Result<(), TrackerError> {
        self.resume_or_advance(&now, &session, self.source_connected)?;
        self.session = Some(session);
        self.reconcile(&now, "session_changed")
    }

    pub fn mark_event_gap(&mut self, reason: &str) -> Result<(), TrackerError> {
        if !self.gap_open
            && let Some(last_sample) = &self.last_sample
        {
            self.store.begin_gap(reason, last_sample)?;
            self.gap_open = true;
        }
        self.close_active(reason)?;
        self.source_connected = false;
        self.focus = None;
        Ok(())
    }

    pub fn mark_session_unavailable(&mut self, reason: &str) -> Result<(), TrackerError> {
        if !self.gap_open
            && let Some(last_sample) = &self.last_sample
        {
            self.store.begin_gap(reason, last_sample)?;
            self.gap_open = true;
        }
        self.close_active(reason)?;
        self.session = None;
        Ok(())
    }

    /// Pauses accounting at the monotonic time an authoritative logind property
    /// edge is received. The previous session state is valid up to that edge;
    /// the fresh state is probed before accounting can resume.
    pub fn observe_session_edge(&mut self, now: ClockSample) -> Result<(), TrackerError> {
        if let Some(session) = self.session.clone() {
            self.advance(&now, &session)?;
        } else {
            self.last_sample = Some(now);
        }
        self.close_active("logind_session_edge")?;
        self.session = None;
        Ok(())
    }

    pub fn store(&self) -> &UsageStore {
        &self.store
    }

    pub fn store_mut(&mut self) -> &mut UsageStore {
        &mut self.store
    }

    pub fn into_store(mut self) -> Result<UsageStore, TrackerError> {
        self.close_active("tracker_stopped")?;
        // UsageTracker has no Drop implementation, so moving the store is safe.
        Ok(self.store)
    }

    fn advance(
        &mut self,
        now: &ClockSample,
        next_session: &SessionSnapshot,
    ) -> Result<(), TrackerError> {
        let Some(previous) = self.last_sample.clone() else {
            self.last_sample = Some(now.clone());
            return Ok(());
        };
        if now.monotonic_ns == previous.monotonic_ns
            && now.wall_utc_ms == previous.wall_utc_ms
            && now.boot_id == previous.boot_id
        {
            return Ok(());
        }
        let gap = now.monotonic_ns.checked_sub(previous.monotonic_ns);
        if gap.is_none() || gap.is_some_and(|value| value > self.config.max_event_gap_ns) {
            self.store
                .record_gap_interval("event_gap", &previous, now)?;
            self.close_active("event_gap")?;
            self.last_sample = Some(now.clone());
            return Ok(());
        }

        if self.active.is_some() {
            if split_into_local_minutes(&previous, now).is_err() {
                self.store
                    .record_gap_interval("clock_discontinuity", &previous, now)?;
                self.close_active("clock_discontinuity")?;
                self.last_sample = Some(now.clone());
                return Ok(());
            }
            if next_session.permits_accounting() {
                let end = now.clone();
                let active = self.active.as_ref().expect("checked above");
                match split_into_local_minutes(&active.last_counted, &end) {
                    Ok(slices) => {
                        self.store.append_segment(
                            active.id,
                            &active.identity.app_id,
                            &end,
                            &slices,
                        )?;
                        self.active.as_mut().expect("checked above").last_counted = end;
                    }
                    Err(error) => {
                        self.store
                            .record_gap_interval("clock_discontinuity", &previous, now)?;
                        self.close_active("clock_discontinuity")?;
                        self.last_sample = Some(now.clone());
                        return if matches!(error, BucketError::MonotonicRegression) {
                            Err(TrackerError::Clock(error))
                        } else {
                            Ok(())
                        };
                    }
                }
            }
        }
        self.last_sample = Some(now.clone());
        Ok(())
    }

    fn resume_or_advance(
        &mut self,
        now: &ClockSample,
        session: &SessionSnapshot,
        authoritative_sources_ready: bool,
    ) -> Result<(), TrackerError> {
        if self.gap_open {
            if authoritative_sources_ready {
                self.store.close_open_gaps(now)?;
                self.gap_open = false;
            }
            self.last_sample = Some(now.clone());
            return Ok(());
        }
        self.advance(now, session)
    }

    fn reconcile(&mut self, now: &ClockSample, close_reason: &str) -> Result<(), TrackerError> {
        let desired = self
            .source_connected
            .then_some(())
            .and(self.session.as_ref())
            .filter(|session| session.permits_accounting())
            .and_then(|_| self.focus.clone());
        let active_matches = self
            .active
            .as_ref()
            .zip(desired.as_ref())
            .is_some_and(|(active, desired)| active.identity == *desired);
        if active_matches {
            return Ok(());
        }
        self.close_active(close_reason)?;
        if let Some(identity) = desired {
            let id = self.store.start_interval(&identity, now)?;
            self.active = Some(ActiveInterval {
                id,
                identity,
                last_counted: now.clone(),
            });
        }
        Ok(())
    }

    fn close_active(&mut self, reason: &str) -> Result<(), TrackerError> {
        if let Some(active) = self.active.take() {
            self.store
                .close_interval(active.id, &active.last_counted, reason)?;
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum TrackerError {
    #[error(transparent)]
    Store(#[from] UsageStoreError),
    #[error(transparent)]
    Clock(#[from] BucketError),
}

#[cfg(test)]
mod tests {
    use crate::{AggregateKind, UsageStore};

    use super::*;

    const BASE_WALL_MS: i64 = 1_767_225_600_000; // 2026-01-01T00:00:00Z

    fn sample(seconds: u64) -> ClockSample {
        ClockSample {
            boot_id: "boot-a".into(),
            monotonic_ns: seconds * 1_000_000_000,
            wall_utc_ms: BASE_WALL_MS + i64::try_from(seconds).unwrap() * 1_000,
            utc_offset_seconds: 0,
            timezone_id: "UTC".into(),
        }
    }

    fn active_session() -> SessionSnapshot {
        SessionSnapshot {
            active: true,
            locked: false,
            idle: false,
        }
    }

    fn focus() -> WindowIdentity {
        WindowIdentity {
            window_id: 1,
            app_id: "terminal".into(),
            pid: Some(100),
        }
    }

    fn tracker() -> (tempfile::TempDir, UsageTracker) {
        let directory = tempfile::tempdir().unwrap();
        let store = UsageStore::open(directory.path().join("usage.sqlite3"), &sample(0)).unwrap();
        (
            directory,
            UsageTracker::new(store, TrackerConfig::default()),
        )
    }

    #[test]
    fn duplicate_focus_does_not_split_interval_or_double_count() {
        let (_directory, mut tracker) = tracker();
        tracker
            .observe_focus(Some(focus()), active_session(), sample(0))
            .unwrap();
        tracker
            .observe_focus(Some(focus()), active_session(), sample(5))
            .unwrap();
        tracker.checkpoint(active_session(), sample(10)).unwrap();
        assert_eq!(tracker.store().interval_count().unwrap(), 1);
        assert_eq!(
            tracker
                .store()
                .aggregate_duration_ns(AggregateKind::Daily, "terminal", "2026-01-01")
                .unwrap(),
            10_000_000_000
        );
        assert_eq!(
            tracker
                .store()
                .aggregate_duration_ns(AggregateKind::Minute, "terminal", "2026-01-01T00:00+00:00")
                .unwrap(),
            10_000_000_000
        );
        assert_eq!(
            tracker
                .store()
                .aggregate_duration_ns(AggregateKind::Weekly, "terminal", "2026-W01")
                .unwrap(),
            10_000_000_000
        );
    }

    #[test]
    fn foreground_reading_without_input_remains_accounted() {
        let (_directory, mut tracker) = tracker();
        tracker
            .observe_focus(Some(focus()), active_session(), sample(0))
            .unwrap();
        tracker.checkpoint(active_session(), sample(10)).unwrap();
        tracker.checkpoint(active_session(), sample(20)).unwrap();
        assert_eq!(
            tracker
                .store()
                .aggregate_duration_ns(AggregateKind::Daily, "terminal", "2026-01-01")
                .unwrap(),
            20_000_000_000
        );
        assert_eq!(tracker.store().open_interval_count().unwrap(), 1);
    }

    #[test]
    fn lock_transition_drops_uncertain_polling_interval() {
        let (_directory, mut tracker) = tracker();
        tracker
            .observe_focus(Some(focus()), active_session(), sample(0))
            .unwrap();
        tracker.checkpoint(active_session(), sample(10)).unwrap();
        let locked = SessionSnapshot {
            active: true,
            locked: true,
            idle: false,
        };
        tracker.checkpoint(locked, sample(20)).unwrap();
        assert_eq!(
            tracker
                .store()
                .aggregate_duration_ns(AggregateKind::Daily, "terminal", "2026-01-01")
                .unwrap(),
            10_000_000_000
        );
    }

    #[test]
    fn idle_transition_stops_accounting_until_activity_resumes() {
        let (_directory, mut tracker) = tracker();
        tracker
            .observe_focus(Some(focus()), active_session(), sample(0))
            .unwrap();
        tracker.checkpoint(active_session(), sample(10)).unwrap();

        tracker.observe_session_edge(sample(12)).unwrap();
        let idle = SessionSnapshot {
            active: true,
            locked: false,
            idle: true,
        };
        tracker.checkpoint(idle, sample(12)).unwrap();
        tracker.checkpoint(active_session(), sample(20)).unwrap();
        tracker.checkpoint(active_session(), sample(25)).unwrap();

        assert_eq!(
            tracker
                .store()
                .aggregate_duration_ns(AggregateKind::Daily, "terminal", "2026-01-01")
                .unwrap(),
            17_000_000_000
        );
    }

    #[test]
    fn authoritative_session_edge_pauses_without_degrading_coverage() {
        let (_directory, mut tracker) = tracker();
        tracker
            .observe_focus(Some(focus()), active_session(), sample(0))
            .unwrap();
        tracker.checkpoint(active_session(), sample(10)).unwrap();

        tracker.observe_session_edge(sample(12)).unwrap();
        assert_eq!(tracker.store().open_interval_count().unwrap(), 0);
        assert_eq!(tracker.store().coverage_state().unwrap().event_gap_count, 0);

        tracker.checkpoint(active_session(), sample(20)).unwrap();
        tracker.checkpoint(active_session(), sample(25)).unwrap();
        assert_eq!(
            tracker
                .store()
                .aggregate_duration_ns(AggregateKind::Daily, "terminal", "2026-01-01")
                .unwrap(),
            17_000_000_000
        );
    }

    #[test]
    fn stale_gap_is_not_backfilled() {
        let (_directory, mut tracker) = tracker();
        tracker
            .observe_focus(Some(focus()), active_session(), sample(0))
            .unwrap();
        tracker.checkpoint(active_session(), sample(10)).unwrap();
        tracker.checkpoint(active_session(), sample(50)).unwrap();
        tracker.checkpoint(active_session(), sample(55)).unwrap();
        assert_eq!(tracker.store().interval_count().unwrap(), 2);
        assert_eq!(
            tracker
                .store()
                .aggregate_duration_ns(AggregateKind::Daily, "terminal", "2026-01-01")
                .unwrap(),
            15_000_000_000
        );
        assert_eq!(tracker.store().coverage_state().unwrap().event_gap_count, 1);
    }

    #[test]
    fn reconnect_closes_the_open_gap_without_recording_it_twice() {
        let (_directory, mut tracker) = tracker();
        tracker
            .observe_focus(Some(focus()), active_session(), sample(0))
            .unwrap();
        tracker.checkpoint(active_session(), sample(10)).unwrap();
        tracker.mark_event_gap("niri_disconnected").unwrap();
        tracker
            .observe_focus(Some(focus()), active_session(), sample(50))
            .unwrap();
        tracker.checkpoint(active_session(), sample(55)).unwrap();

        assert_eq!(tracker.store().coverage_state().unwrap().event_gap_count, 1);
        assert_eq!(
            tracker
                .store()
                .aggregate_duration_ns(AggregateKind::Daily, "terminal", "2026-01-01")
                .unwrap(),
            15_000_000_000
        );
    }

    #[test]
    fn startup_gap_remains_open_until_the_first_niri_focus_snapshot() {
        let (_directory, mut tracker) = tracker();
        tracker
            .store_mut()
            .begin_gap("usage_daemon_starting", &sample(0))
            .unwrap();
        tracker.gap_open = true;

        tracker.checkpoint(active_session(), sample(10)).unwrap();
        assert!(tracker.store().has_open_gap().unwrap());
        assert_eq!(tracker.store().open_interval_count().unwrap(), 0);

        tracker
            .observe_focus(Some(focus()), active_session(), sample(20))
            .unwrap();
        assert!(!tracker.store().has_open_gap().unwrap());
        tracker.checkpoint(active_session(), sample(25)).unwrap();
        assert_eq!(
            tracker
                .store()
                .aggregate_duration_ns(AggregateKind::Daily, "terminal", "2026-01-01")
                .unwrap(),
            5_000_000_000
        );
    }

    #[test]
    fn timezone_change_drops_ambiguous_segment_and_restarts() {
        let (_directory, mut tracker) = tracker();
        tracker
            .observe_focus(Some(focus()), active_session(), sample(0))
            .unwrap();
        let mut changed = sample(10);
        changed.utc_offset_seconds = 3_600;
        changed.timezone_id = "Europe/Paris".into();
        tracker
            .checkpoint(active_session(), changed.clone())
            .unwrap();
        let mut next = sample(15);
        next.utc_offset_seconds = 3_600;
        next.timezone_id = "Europe/Paris".into();
        tracker.checkpoint(active_session(), next).unwrap();
        assert_eq!(tracker.store().interval_count().unwrap(), 2);
        assert_eq!(
            tracker
                .store()
                .aggregate_duration_ns(AggregateKind::Daily, "terminal", "2026-01-01")
                .unwrap(),
            5_000_000_000
        );
        assert_eq!(tracker.store().coverage_state().unwrap().event_gap_count, 1);
    }

    #[test]
    fn timezone_change_cannot_be_hidden_by_a_lock_transition() {
        let (_directory, mut tracker) = tracker();
        tracker
            .observe_focus(Some(focus()), active_session(), sample(0))
            .unwrap();
        let mut changed = sample(10);
        changed.utc_offset_seconds = 3_600;
        changed.timezone_id = "Europe/Paris".into();
        let locked = SessionSnapshot {
            active: true,
            locked: true,
            idle: false,
        };
        tracker.checkpoint(locked, changed).unwrap();
        assert_eq!(
            tracker
                .store()
                .aggregate_duration_ns(AggregateKind::Daily, "terminal", "2026-01-01")
                .unwrap(),
            0
        );
    }

    #[test]
    fn suspend_wall_gap_is_not_counted_as_usage() {
        let (_directory, mut tracker) = tracker();
        tracker
            .observe_focus(Some(focus()), active_session(), sample(0))
            .unwrap();
        let mut resumed = sample(10);
        resumed.wall_utc_ms += 3_600_000;
        tracker
            .checkpoint(active_session(), resumed.clone())
            .unwrap();
        let mut next = resumed.clone();
        next.monotonic_ns += 5_000_000_000;
        next.wall_utc_ms += 5_000;
        tracker.checkpoint(active_session(), next).unwrap();
        assert_eq!(
            tracker
                .store()
                .aggregate_duration_ns(AggregateKind::Daily, "terminal", "2026-01-01")
                .unwrap(),
            5_000_000_000
        );
    }

    #[test]
    fn boot_change_with_monotonic_reset_drops_the_restart_gap() {
        let (_directory, mut tracker) = tracker();
        tracker
            .observe_focus(Some(focus()), active_session(), sample(0))
            .unwrap();
        tracker.checkpoint(active_session(), sample(10)).unwrap();

        let mut restarted = sample(20);
        restarted.boot_id = "boot-b".into();
        restarted.monotonic_ns = 1_000_000_000;
        tracker
            .checkpoint(active_session(), restarted.clone())
            .unwrap();
        let mut after_restart = restarted;
        after_restart.monotonic_ns += 5_000_000_000;
        after_restart.wall_utc_ms += 5_000;
        tracker.checkpoint(active_session(), after_restart).unwrap();

        assert_eq!(tracker.store().coverage_state().unwrap().event_gap_count, 1);
        assert_eq!(
            tracker
                .store()
                .aggregate_duration_ns(AggregateKind::Daily, "terminal", "2026-01-01")
                .unwrap(),
            15_000_000_000
        );
    }

    #[test]
    fn tracker_persists_cross_midnight_duration_in_each_daily_bucket() {
        let (_directory, mut tracker) = tracker();
        tracker
            .observe_focus(Some(focus()), active_session(), sample(86_395))
            .unwrap();
        tracker
            .checkpoint(active_session(), sample(86_405))
            .unwrap();

        assert_eq!(
            tracker
                .store()
                .aggregate_duration_ns(AggregateKind::Daily, "terminal", "2026-01-01")
                .unwrap(),
            5_000_000_000
        );
        assert_eq!(
            tracker
                .store()
                .aggregate_duration_ns(AggregateKind::Daily, "terminal", "2026-01-02")
                .unwrap(),
            5_000_000_000
        );
    }
}
