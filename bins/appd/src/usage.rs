use localdesk_domain::{
    CapabilityAvailability, CapabilityRuntimeState, USAGE_DEFINITION, USAGE_SCHEMA_VERSION,
    UsageApplicationDuration, UsageCoverage, UsagePeriod, UsageSummary, UsageSummaryQuery,
};
use localdesk_usage::{
    ClockSource, LogindEventStream, LogindProbe, NiriEventStream, NiriUpdate, RetentionPolicy,
    SegmentedAggregateQuery, SummaryBucket, SummaryKind, SystemClock, TrackerConfig,
    UsageCoverageState, UsageQueryInterrupt, UsageReader, UsageStore, UsageTracker,
    WaylandIdleEventStream,
};
use nix::unistd::Uid;
use std::{
    fs::{self, DirBuilder, OpenOptions},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use tokio::sync::{oneshot, watch};
use uuid::Uuid;

const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(5);
const RETENTION_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const QUERY_DEADLINE: Duration = Duration::from_secs(5);
// v3 did not have an authoritative compositor idle source. Keep those rows
// intact and start a clean epoch for the input-active definition.
const DATABASE_FILE: &str = "usage-v4.sqlite3";
const STATE_DIRECTORY: &str = "localdesk";

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct UsageRuntimeError {
    pub code: &'static str,
    pub reason: &'static str,
    pub retryable: bool,
}

impl UsageRuntimeError {
    const fn new(code: &'static str, reason: &'static str, retryable: bool) -> Self {
        Self {
            code,
            reason,
            retryable,
        }
    }
}

#[derive(Clone)]
pub struct UsageHandle {
    shared: Arc<RwLock<UsageRuntimeState>>,
}

struct UsageRuntimeState {
    capability: CapabilityRuntimeState,
    niri_connected: bool,
    session_available: bool,
    last_checkpoint_unix_ms: Option<i64>,
    event_gap_count: u64,
    writer_client: Option<mpsc::Sender<WriterCommand>>,
    query_client: Option<QueryClient>,
}

#[derive(Clone)]
struct QueryClient {
    sender: mpsc::SyncSender<QueryCommand>,
    busy: Arc<AtomicBool>,
    next_token: Arc<AtomicU64>,
    control: Arc<QueryControl>,
}

impl QueryClient {
    fn reserve(&self) -> Result<u64, UsageRuntimeError> {
        self.busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| UsageRuntimeError::new("usage_provider_busy", "usage_query_busy", true))?;
        Ok(self.next_token.fetch_add(1, Ordering::Relaxed))
    }

    #[cfg(test)]
    fn try_send(
        &self,
        query: UsageSummaryQuery,
        reply: oneshot::Sender<Result<UsageSummary, UsageRuntimeError>>,
    ) -> Result<u64, UsageRuntimeError> {
        let token = self.reserve()?;
        self.send_reserved(token, query, reply)?;
        Ok(token)
    }

    fn send_reserved(
        &self,
        token: u64,
        query: UsageSummaryQuery,
        reply: oneshot::Sender<Result<UsageSummary, UsageRuntimeError>>,
    ) -> Result<(), UsageRuntimeError> {
        match self.sender.try_send(QueryCommand::Query {
            token,
            query,
            reply,
        }) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(_)) => {
                self.busy.store(false, Ordering::Release);
                Err(UsageRuntimeError::new(
                    "usage_provider_busy",
                    "usage_query_busy",
                    true,
                ))
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                self.busy.store(false, Ordering::Release);
                Err(UsageRuntimeError::new(
                    "usage_provider_unavailable",
                    "usage_query_worker_unavailable",
                    true,
                ))
            }
        }
    }

    fn release_reservation(&self) {
        self.busy.store(false, Ordering::Release);
    }

    fn cancel(&self, token: u64) {
        self.control.cancel(token);
    }
}

struct QueryControl {
    current_token: Mutex<Option<u64>>,
    interrupt: UsageQueryInterrupt,
    cancelled_token: AtomicU64,
    stopping: AtomicBool,
}

impl QueryControl {
    fn begin(&self, token: u64) -> bool {
        if let Ok(mut current) = self.current_token.lock() {
            *current = Some(token);
        }
        if self.stopping.load(Ordering::Acquire)
            || self.cancelled_token.load(Ordering::Acquire) == token
        {
            self.finish(token);
            return false;
        }
        true
    }

    fn finish(&self, token: u64) {
        if let Ok(mut current) = self.current_token.lock()
            && *current == Some(token)
        {
            *current = None;
        }
        let _ =
            self.cancelled_token
                .compare_exchange(token, 0, Ordering::AcqRel, Ordering::Acquire);
    }

    fn cancel(&self, token: u64) {
        self.cancelled_token.store(token, Ordering::Release);
        if let Ok(current) = self.current_token.lock()
            && *current == Some(token)
        {
            self.interrupt.interrupt();
        }
    }

    fn cancel_current(&self) {
        self.stopping.store(true, Ordering::Release);
        if let Ok(current) = self.current_token.lock()
            && current.is_some()
        {
            self.interrupt.interrupt();
        }
    }
}

enum QueryCommand {
    Query {
        token: u64,
        query: UsageSummaryQuery,
        reply: oneshot::Sender<Result<UsageSummary, UsageRuntimeError>>,
    },
    Shutdown,
}

struct QueryRuntime {
    client: QueryClient,
    worker: thread::JoinHandle<()>,
}

impl QueryRuntime {
    fn shutdown(self) {
        self.client.control.cancel_current();
        // A queued query is itself sufficient to wake the worker: `stopping`
        // makes it fail closed and the worker exits before receiving again.
        // Never block the writer shutdown waiting for capacity in this queue.
        let _ = self.client.sender.try_send(QueryCommand::Shutdown);
        let _ = self.worker.join();
    }
}

impl UsageHandle {
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn unavailable_for_test(reason: &'static str) -> Self {
        Self {
            shared: Arc::new(RwLock::new(UsageRuntimeState {
                capability: CapabilityRuntimeState::unreachable(reason),
                niri_connected: false,
                session_available: false,
                last_checkpoint_unix_ms: None,
                event_gap_count: 0,
                writer_client: None,
                query_client: None,
            })),
        }
    }

    pub fn capability_state(&self) -> CapabilityRuntimeState {
        self.shared
            .read()
            .map(|state| state.capability.clone())
            .unwrap_or_else(|_| CapabilityRuntimeState::unreachable("usage_store_poisoned"))
    }

    pub async fn summary(
        &self,
        query: UsageSummaryQuery,
    ) -> Result<UsageSummary, UsageRuntimeError> {
        query.validate().map_err(|_| {
            UsageRuntimeError::new("usage_query_invalid", "usage_bucket_key_invalid", false)
        })?;
        let (writer_client, query_client) = self
            .shared
            .read()
            .map_err(|_| {
                UsageRuntimeError::new("usage_runtime_error", "usage_store_poisoned", false)
            })
            .map(|state| (state.writer_client.clone(), state.query_client.clone()))?;
        let query_client = query_client.ok_or_else(|| {
            let reason = self
                .shared
                .read()
                .ok()
                .map(|state| stable_runtime_reason(&state.capability.reason))
                .unwrap_or("usage_worker_unavailable");
            UsageRuntimeError::new("usage_provider_unavailable", reason, true)
        })?;
        let deadline = tokio::time::Instant::now() + QUERY_DEADLINE;
        let token = query_client.reserve()?;
        match query_targets_current_bucket(&query) {
            Ok(true) => {
                if let Err(error) = checkpoint_writer(writer_client, deadline).await {
                    query_client.release_reservation();
                    return Err(error);
                }
            }
            Ok(false) => {}
            Err(error) => {
                query_client.release_reservation();
                return Err(error);
            }
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        query_client.send_reserved(token, query, reply_tx)?;
        tokio::time::timeout_at(deadline, reply_rx)
            .await
            .map_err(|_| {
                query_client.cancel(token);
                UsageRuntimeError::new("usage_provider_timeout", "usage_query_timeout", true)
            })?
            .map_err(|_| {
                UsageRuntimeError::new(
                    "usage_provider_unavailable",
                    "usage_worker_unavailable",
                    true,
                )
            })?
    }
}

fn query_targets_current_bucket(query: &UsageSummaryQuery) -> Result<bool, UsageRuntimeError> {
    let mut clock = SystemClock;
    let sample = clock.sample().map_err(|_| {
        UsageRuntimeError::new("usage_query_failed", "usage_clock_unavailable", true)
    })?;
    let kind = match query.period {
        UsagePeriod::Daily => SummaryKind::Daily,
        UsagePeriod::Weekly => SummaryKind::Weekly,
    };
    let current = SummaryBucket::for_sample(kind, &sample).map_err(|_| {
        UsageRuntimeError::new("usage_query_failed", "usage_clock_unavailable", true)
    })?;
    Ok(query.bucket_key == current.bucket_key)
}

async fn checkpoint_writer(
    writer: Option<mpsc::Sender<WriterCommand>>,
    deadline: tokio::time::Instant,
) -> Result<(), UsageRuntimeError> {
    let writer = writer.ok_or_else(|| {
        UsageRuntimeError::new(
            "usage_provider_unavailable",
            "usage_worker_unavailable",
            true,
        )
    })?;
    let (reply_tx, reply_rx) = oneshot::channel();
    writer
        .send(WriterCommand::Checkpoint { reply: reply_tx })
        .map_err(|_| {
            UsageRuntimeError::new(
                "usage_provider_unavailable",
                "usage_worker_unavailable",
                true,
            )
        })?;
    tokio::time::timeout_at(deadline, reply_rx)
        .await
        .map_err(|_| {
            UsageRuntimeError::new("usage_provider_timeout", "usage_checkpoint_timeout", true)
        })?
        .map_err(|_| {
            UsageRuntimeError::new(
                "usage_provider_unavailable",
                "usage_worker_unavailable",
                true,
            )
        })
}

pub struct UsageSupervisor {
    shared: Arc<RwLock<UsageRuntimeState>>,
    database_path: Result<PathBuf, UsageRuntimeError>,
}

impl UsageSupervisor {
    pub fn from_environment() -> Self {
        let shared = Arc::new(RwLock::new(UsageRuntimeState {
            capability: CapabilityRuntimeState::degraded("usage_warming_up"),
            niri_connected: false,
            session_available: false,
            last_checkpoint_unix_ms: None,
            event_gap_count: 0,
            writer_client: None,
            query_client: None,
        }));
        Self {
            shared,
            database_path: usage_database_path_from_environment(),
        }
    }

    pub fn handle(&self) -> UsageHandle {
        UsageHandle {
            shared: Arc::clone(&self.shared),
        }
    }

    pub async fn run(self, mut shutdown: watch::Receiver<bool>) {
        let database_path = match self.database_path {
            Ok(path) => path,
            Err(error) => {
                publish_unavailable(&self.shared, error.reason);
                wait_for_shutdown(&mut shutdown).await;
                return;
            }
        };
        let (command_tx, command_rx) = mpsc::channel();
        if let Ok(mut state) = self.shared.write() {
            state.writer_client = Some(command_tx.clone());
        }
        let worker_shared = Arc::clone(&self.shared);
        let (done_tx, mut done_rx) = oneshot::channel();
        let worker = match thread::Builder::new()
            .name("localdesk-usage".to_owned())
            .spawn(move || {
                usage_worker(database_path, command_rx, &worker_shared);
                let _ = done_tx.send(());
            }) {
            Ok(worker) => worker,
            Err(_) => {
                publish_unavailable(&self.shared, "usage_worker_start_failed");
                wait_for_shutdown(&mut shutdown).await;
                return;
            }
        };

        let worker_finished = tokio::select! {
            _ = &mut done_rx => true,
            _ = wait_for_shutdown(&mut shutdown) => false,
        };
        if !worker_finished {
            let _ = command_tx.send(WriterCommand::Shutdown);
        }
        if let Ok(mut state) = self.shared.write() {
            state.writer_client = None;
            state.query_client = None;
            if !worker_finished {
                state.capability = CapabilityRuntimeState::unreachable("usage_shutting_down");
            }
        }
        let _ = tokio::task::spawn_blocking(move || worker.join()).await;
    }
}

enum WriterCommand {
    Checkpoint { reply: oneshot::Sender<()> },
    Shutdown,
}

struct WorkerState {
    tracker: UsageTracker,
    clock: SystemClock,
    logind: LogindProbe,
    logind_events: Option<LogindEventStream>,
    wayland_idle: Option<WaylandIdleEventStream>,
    input_idle: bool,
    niri: Option<NiriEventStream>,
    niri_snapshot_seen: bool,
    session_available: bool,
    last_checkpoint_unix_ms: Option<i64>,
    reconnect_delay: Duration,
    next_reconnect: Instant,
    logind_reconnect_delay: Duration,
    next_logind_reconnect: Instant,
    idle_reconnect_delay: Duration,
    next_idle_reconnect: Instant,
    next_checkpoint: Instant,
    next_retention: Instant,
}

fn usage_worker(
    database_path: PathBuf,
    commands: mpsc::Receiver<WriterCommand>,
    shared: &Arc<RwLock<UsageRuntimeState>>,
) {
    let mut clock = SystemClock;
    let recovered_at = match clock.sample() {
        Ok(sample) => sample,
        Err(_) => {
            publish_unavailable(shared, "usage_clock_unavailable");
            return;
        }
    };
    let mut store = match UsageStore::open(&database_path, &recovered_at) {
        Ok(store) => store,
        Err(error) => {
            publish_unavailable(shared, usage_store_open_reason(&error));
            return;
        }
    };
    if store
        .apply_retention(recovered_at.wall_utc_ms, RetentionPolicy::default())
        .is_err()
    {
        publish_unavailable(shared, "usage_retention_failed");
        return;
    }
    if store
        .begin_gap("usage_daemon_starting", &recovered_at)
        .is_err()
    {
        publish_unavailable(shared, "usage_coverage_write_failed");
        return;
    }
    let logind = match LogindProbe::from_environment() {
        Ok(probe) => probe,
        Err(_) => {
            publish_unsupported(shared, "logind_session_unavailable");
            return;
        }
    };
    let query_runtime = match start_query_runtime(&database_path, shared) {
        Ok(runtime) => runtime,
        Err(error) => {
            publish_unavailable(shared, error.reason);
            return;
        }
    };
    let coverage = store.coverage_state().unwrap_or_default();
    let mut worker = WorkerState {
        tracker: UsageTracker::new(store, TrackerConfig::default()),
        clock,
        logind,
        logind_events: None,
        wayland_idle: None,
        input_idle: false,
        niri: None,
        niri_snapshot_seen: false,
        session_available: false,
        last_checkpoint_unix_ms: None,
        reconnect_delay: POLL_INTERVAL,
        next_reconnect: Instant::now(),
        logind_reconnect_delay: POLL_INTERVAL,
        next_logind_reconnect: Instant::now(),
        idle_reconnect_delay: POLL_INTERVAL,
        next_idle_reconnect: Instant::now(),
        next_checkpoint: Instant::now(),
        next_retention: Instant::now() + RETENTION_INTERVAL,
    };
    publish_worker_state(
        shared,
        &worker,
        &coverage,
        "usage_waiting_for_niri_snapshot",
    );

    'worker: loop {
        match commands.try_recv() {
            Ok(WriterCommand::Checkpoint { reply }) => {
                checkpoint_current_usage(&mut worker, shared);
                let _ = reply.send(());
            }
            Ok(WriterCommand::Shutdown) | Err(mpsc::TryRecvError::Disconnected) => break 'worker,
            Err(mpsc::TryRecvError::Empty) => {}
        }
        let now = Instant::now();
        if worker.wayland_idle.is_none() && now >= worker.next_idle_reconnect {
            match WaylandIdleEventStream::connect() {
                Ok(stream) => {
                    worker.input_idle = stream.is_idle();
                    worker.wayland_idle = Some(stream);
                    worker.idle_reconnect_delay = POLL_INTERVAL;
                    refresh_session(&mut worker, shared);
                }
                Err(_) => {
                    worker.session_available = false;
                    worker.next_idle_reconnect = now + worker.idle_reconnect_delay;
                    worker.idle_reconnect_delay =
                        (worker.idle_reconnect_delay * 2).min(MAX_RECONNECT_DELAY);
                    publish_current_state(&worker, shared, "wayland_idle_event_stream_unavailable");
                }
            }
        }
        poll_wayland_idle_edges(&mut worker, shared);
        if worker.logind_events.is_none() && now >= worker.next_logind_reconnect {
            match LogindEventStream::spawn(worker.logind.session_id()) {
                Ok(stream) => {
                    worker.logind_events = Some(stream);
                    worker.logind_reconnect_delay = POLL_INTERVAL;
                    worker.session_available = false;
                    refresh_session(&mut worker, shared);
                }
                Err(_) => {
                    worker.session_available = false;
                    worker.next_logind_reconnect = now + worker.logind_reconnect_delay;
                    worker.logind_reconnect_delay =
                        (worker.logind_reconnect_delay * 2).min(MAX_RECONNECT_DELAY);
                    publish_current_state(&worker, shared, "logind_event_stream_unavailable");
                }
            }
        }
        poll_logind_edges(&mut worker, shared);
        if worker.niri.is_none() && now >= worker.next_reconnect {
            match NiriEventStream::spawn() {
                Ok(stream) => {
                    worker.niri = Some(stream);
                    worker.niri_snapshot_seen = false;
                    worker.reconnect_delay = POLL_INTERVAL;
                    publish_current_state(&worker, shared, "usage_waiting_for_niri_snapshot");
                }
                Err(_) => {
                    worker.next_reconnect = now + worker.reconnect_delay;
                    worker.reconnect_delay = (worker.reconnect_delay * 2).min(MAX_RECONNECT_DELAY);
                    publish_current_state(&worker, shared, "niri_event_stream_unavailable");
                }
            }
        }
        if now >= worker.next_checkpoint {
            refresh_session(&mut worker, shared);
            worker.next_checkpoint = Instant::now() + CHECKPOINT_INTERVAL;
        }
        if now >= worker.next_retention {
            if let Ok(sample) = worker.clock.sample()
                && worker
                    .tracker
                    .store_mut()
                    .apply_retention(sample.wall_utc_ms, RetentionPolicy::default())
                    .is_err()
            {
                publish_current_state(&worker, shared, "usage_retention_failed");
            }
            worker.next_retention = Instant::now() + RETENTION_INTERVAL;
        }

        let update = worker
            .niri
            .as_mut()
            .map(|stream| stream.poll_update(POLL_INTERVAL));
        match update {
            Some(Ok(Some(NiriUpdate::FocusChanged(focus)))) => {
                worker.niri_snapshot_seen = true;
                refresh_session_with_focus(&mut worker, focus, shared);
            }
            Some(Ok(Some(NiriUpdate::StateChanged | NiriUpdate::Ignored))) | Some(Ok(None)) => {}
            Some(Err(_)) => {
                let _ = worker
                    .tracker
                    .mark_event_gap("niri_event_stream_disconnected");
                worker.niri = None;
                worker.niri_snapshot_seen = false;
                worker.next_reconnect = Instant::now() + worker.reconnect_delay;
                worker.reconnect_delay = (worker.reconnect_delay * 2).min(MAX_RECONNECT_DELAY);
                publish_current_state(&worker, shared, "niri_event_stream_disconnected");
            }
            None => match commands.recv_timeout(POLL_INTERVAL) {
                Ok(WriterCommand::Checkpoint { reply }) => {
                    checkpoint_current_usage(&mut worker, shared);
                    let _ = reply.send(());
                }
                Ok(WriterCommand::Shutdown) => break 'worker,
                Err(mpsc::RecvTimeoutError::Disconnected) => break 'worker,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            },
        }
    }
    if let Ok(mut state) = shared.write() {
        state.query_client = None;
    }
    checkpoint_current_usage(&mut worker, shared);
    let _ = worker.tracker.mark_event_gap("usage_daemon_stopped");
    query_runtime.shutdown();
    let _ = worker.tracker.into_store();
}

fn checkpoint_current_usage(worker: &mut WorkerState, shared: &Arc<RwLock<UsageRuntimeState>>) {
    poll_wayland_idle_edges(worker, shared);
    poll_logind_edges(worker, shared);

    loop {
        let update = worker
            .niri
            .as_mut()
            .map(|stream| stream.poll_update(Duration::ZERO));
        match update {
            Some(Ok(Some(NiriUpdate::FocusChanged(focus)))) => {
                worker.niri_snapshot_seen = true;
                refresh_session_with_focus(worker, focus, shared);
            }
            Some(Ok(Some(NiriUpdate::StateChanged | NiriUpdate::Ignored))) => {}
            Some(Ok(None)) | None => break,
            Some(Err(_)) => {
                let _ = worker
                    .tracker
                    .mark_event_gap("niri_event_stream_disconnected");
                worker.niri = None;
                worker.niri_snapshot_seen = false;
                worker.next_reconnect = Instant::now() + worker.reconnect_delay;
                worker.reconnect_delay = (worker.reconnect_delay * 2).min(MAX_RECONNECT_DELAY);
                publish_current_state(worker, shared, "niri_event_stream_disconnected");
                return;
            }
        }
    }
    refresh_session(worker, shared);
}

fn poll_logind_edges(worker: &mut WorkerState, shared: &Arc<RwLock<UsageRuntimeState>>) {
    match drain_logind_events(worker) {
        Ok(false) => {}
        Ok(true) => {
            pause_for_logind_edge(worker, shared);
            refresh_session(worker, shared);
        }
        Err(()) => disconnect_logind_events(worker, shared),
    }
}

fn poll_wayland_idle_edges(worker: &mut WorkerState, shared: &Arc<RwLock<UsageRuntimeState>>) {
    let changed = worker
        .wayland_idle
        .as_mut()
        .map(|stream| stream.poll_changed(Duration::ZERO));
    match changed {
        Some(Ok(Some(idle))) => {
            pause_for_idle_edge(worker, shared);
            worker.input_idle = idle;
            refresh_session(worker, shared);
        }
        Some(Ok(None)) | None => {}
        Some(Err(_)) => disconnect_wayland_idle(worker, shared),
    }
}

fn refresh_session(worker: &mut WorkerState, shared: &Arc<RwLock<UsageRuntimeState>>) {
    let focus = worker
        .niri
        .as_ref()
        .and_then(|stream| stream.state().focused_identity());
    refresh_session_with_focus(worker, focus, shared);
}

fn refresh_session_with_focus(
    worker: &mut WorkerState,
    focus: Option<localdesk_usage::WindowIdentity>,
    shared: &Arc<RwLock<UsageRuntimeState>>,
) {
    if worker.wayland_idle.is_none() {
        worker.session_available = false;
        publish_current_state(worker, shared, "wayland_idle_event_stream_unavailable");
        return;
    }
    if worker.logind_events.is_none() {
        worker.session_available = false;
        publish_current_state(worker, shared, "logind_event_stream_unavailable");
        return;
    }
    match drain_logind_events(worker) {
        Ok(true) => pause_for_logind_edge(worker, shared),
        Ok(false) => {}
        Err(()) => {
            disconnect_logind_events(worker, shared);
            return;
        }
    }

    let mut session = match worker.logind.probe() {
        Ok(session) => session,
        Err(_) => {
            let _ = worker
                .tracker
                .mark_session_unavailable("logind_probe_unavailable");
            worker.session_available = false;
            publish_current_state(worker, shared, "logind_probe_unavailable");
            return;
        }
    };

    match drain_logind_events(worker) {
        Ok(true) => {
            pause_for_logind_edge(worker, shared);
            worker.next_checkpoint = Instant::now();
            return;
        }
        Ok(false) => {}
        Err(()) => {
            disconnect_logind_events(worker, shared);
            return;
        }
    }

    // The enabling monotonic sample is deliberately taken only after the
    // authoritative loginctl snapshot and a post-probe edge drain.
    let sample = match worker.clock.sample() {
        Ok(sample) => sample,
        Err(_) => {
            publish_current_state(worker, shared, "usage_clock_unavailable");
            return;
        }
    };
    session.idle = worker.input_idle;
    let result = if worker.niri_snapshot_seen {
        worker.tracker.observe_focus(focus, session, sample.clone())
    } else {
        worker.tracker.checkpoint(session, sample.clone())
    };
    if result.is_err() {
        publish_unavailable(shared, "usage_tracker_failed");
        return;
    }
    worker.session_available = true;
    worker.last_checkpoint_unix_ms = Some(sample.wall_utc_ms);
    let reason = if worker.niri_snapshot_seen {
        "usage_tracking_active"
    } else {
        "usage_waiting_for_niri_snapshot"
    };
    publish_current_state(worker, shared, reason);
}

fn drain_logind_events(worker: &mut WorkerState) -> Result<bool, ()> {
    let Some(events) = worker.logind_events.as_mut() else {
        return Err(());
    };
    let mut changed = false;
    loop {
        match events.poll_changed(Duration::ZERO) {
            Ok(true) => changed = true,
            Ok(false) => return Ok(changed),
            Err(_) => return Err(()),
        }
    }
}

fn pause_for_logind_edge(worker: &mut WorkerState, shared: &Arc<RwLock<UsageRuntimeState>>) {
    let sample = match worker.clock.sample() {
        Ok(sample) => sample,
        Err(_) => {
            let _ = worker
                .tracker
                .mark_session_unavailable("usage_clock_unavailable");
            worker.session_available = false;
            publish_current_state(worker, shared, "usage_clock_unavailable");
            return;
        }
    };
    if worker.tracker.observe_session_edge(sample.clone()).is_err() {
        publish_unavailable(shared, "usage_tracker_failed");
        return;
    }
    worker.session_available = false;
    worker.last_checkpoint_unix_ms = Some(sample.wall_utc_ms);
    publish_current_state(worker, shared, "logind_session_state_changed");
}

fn disconnect_logind_events(worker: &mut WorkerState, shared: &Arc<RwLock<UsageRuntimeState>>) {
    let _ = worker
        .tracker
        .mark_session_unavailable("logind_event_stream_disconnected");
    worker.logind_events = None;
    worker.session_available = false;
    worker.next_logind_reconnect = Instant::now() + worker.logind_reconnect_delay;
    worker.logind_reconnect_delay = (worker.logind_reconnect_delay * 2).min(MAX_RECONNECT_DELAY);
    publish_current_state(worker, shared, "logind_event_stream_disconnected");
}

fn pause_for_idle_edge(worker: &mut WorkerState, shared: &Arc<RwLock<UsageRuntimeState>>) {
    let sample = match worker.clock.sample() {
        Ok(sample) => sample,
        Err(_) => {
            let _ = worker
                .tracker
                .mark_session_unavailable("usage_clock_unavailable");
            worker.session_available = false;
            publish_current_state(worker, shared, "usage_clock_unavailable");
            return;
        }
    };
    if worker.tracker.observe_session_edge(sample.clone()).is_err() {
        publish_unavailable(shared, "usage_tracker_failed");
        return;
    }
    worker.session_available = false;
    worker.last_checkpoint_unix_ms = Some(sample.wall_utc_ms);
}

fn disconnect_wayland_idle(worker: &mut WorkerState, shared: &Arc<RwLock<UsageRuntimeState>>) {
    let _ = worker
        .tracker
        .mark_session_unavailable("wayland_idle_event_stream_disconnected");
    worker.wayland_idle = None;
    worker.input_idle = false;
    worker.session_available = false;
    worker.next_idle_reconnect = Instant::now() + worker.idle_reconnect_delay;
    worker.idle_reconnect_delay = (worker.idle_reconnect_delay * 2).min(MAX_RECONNECT_DELAY);
    publish_current_state(worker, shared, "wayland_idle_event_stream_disconnected");
}

fn start_query_runtime(
    database_path: &Path,
    shared: &Arc<RwLock<UsageRuntimeState>>,
) -> Result<QueryRuntime, UsageRuntimeError> {
    let reader = UsageReader::open(database_path).map_err(|_| {
        UsageRuntimeError::new(
            "usage_state_unavailable",
            "usage_query_database_unavailable",
            true,
        )
    })?;
    let control = Arc::new(QueryControl {
        current_token: Mutex::new(None),
        interrupt: reader.interrupt_handle(),
        cancelled_token: AtomicU64::new(0),
        stopping: AtomicBool::new(false),
    });
    let busy = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = mpsc::sync_channel(1);
    let client = QueryClient {
        sender,
        busy: Arc::clone(&busy),
        next_token: Arc::new(AtomicU64::new(1)),
        control: Arc::clone(&control),
    };
    let query_shared = Arc::clone(shared);
    let worker = thread::Builder::new()
        .name("localdesk-usage-query".to_owned())
        .spawn(move || query_worker(reader, receiver, &query_shared, &control, &busy))
        .map_err(|_| {
            UsageRuntimeError::new(
                "usage_provider_unavailable",
                "usage_query_worker_start_failed",
                true,
            )
        })?;
    if let Ok(mut state) = shared.write() {
        state.query_client = Some(client.clone());
    }
    Ok(QueryRuntime { client, worker })
}

fn query_worker(
    reader: UsageReader,
    commands: mpsc::Receiver<QueryCommand>,
    shared: &Arc<RwLock<UsageRuntimeState>>,
    control: &Arc<QueryControl>,
    busy: &Arc<AtomicBool>,
) {
    loop {
        if control.stopping.load(Ordering::Acquire) {
            break;
        }
        let Ok(command) = commands.recv() else {
            break;
        };
        match command {
            QueryCommand::Query {
                token,
                query,
                reply,
            } => {
                let result = if control.begin(token) {
                    build_summary(&reader, shared, query)
                } else {
                    Err(UsageRuntimeError::new(
                        "usage_provider_unavailable",
                        "usage_query_worker_stopping",
                        true,
                    ))
                };
                control.finish(token);
                busy.store(false, Ordering::Release);
                let _ = reply.send(result);
            }
            QueryCommand::Shutdown => break,
        }
    }
    busy.store(false, Ordering::Release);
}

fn build_summary(
    reader: &UsageReader,
    shared: &Arc<RwLock<UsageRuntimeState>>,
    query: UsageSummaryQuery,
) -> Result<UsageSummary, UsageRuntimeError> {
    query.validate().map_err(|_| {
        UsageRuntimeError::new("usage_query_invalid", "usage_bucket_key_invalid", false)
    })?;
    let kind = match query.period {
        UsagePeriod::Daily => SummaryKind::Daily,
        UsagePeriod::Weekly => SummaryKind::Weekly,
    };
    let aggregates = reader
        .aggregate_segments_for_key(kind, &query.bucket_key)
        .map_err(|_| {
            UsageRuntimeError::new("usage_query_failed", "usage_database_query_failed", true)
        })?;
    let coverage = reader
        .coverage_state_for_bucket(kind, &query.bucket_key)
        .map_err(|_| {
            UsageRuntimeError::new("usage_query_failed", "usage_coverage_query_failed", true)
        })?;
    let mut clock = SystemClock;
    let captured = clock.sample().map_err(|_| {
        UsageRuntimeError::new("usage_query_failed", "usage_clock_unavailable", true)
    })?;
    let runtime = shared.read().map_err(|_| {
        UsageRuntimeError::new("usage_runtime_error", "usage_store_poisoned", false)
    })?;
    let bucket_coverage = bucket_coverage(&query, &coverage);
    let (status, reason, retryable) =
        summary_status(&runtime, &coverage, aggregates.truncated, bucket_coverage);
    let applications = project_aggregates(aggregates);
    let summary = UsageSummary {
        schema_version: USAGE_SCHEMA_VERSION,
        snapshot_id: Uuid::new_v4(),
        captured_at_unix_ms: Some(captured.wall_utc_ms),
        query,
        status,
        reason: reason.to_owned(),
        retryable,
        coverage: UsageCoverage {
            status,
            reason: reason.to_owned(),
            niri_event_stream_connected: runtime.niri_connected,
            logind_session_available: runtime.session_available,
            event_gap_count: coverage.event_gap_count,
            last_checkpoint_unix_ms: runtime.last_checkpoint_unix_ms,
            tracking_started_unix_ms: coverage.tracking_started_wall_utc_ms,
            bucket_start_covered: bucket_coverage == BucketCoverage::Complete,
            definition: USAGE_DEFINITION.to_owned(),
        },
        applications,
    };
    summary.validate().map_err(|_| {
        UsageRuntimeError::new("usage_query_failed", "usage_summary_invalid", false)
    })?;
    Ok(summary)
}

fn project_aggregates(aggregates: SegmentedAggregateQuery) -> Vec<UsageApplicationDuration> {
    aggregates
        .entries
        .into_iter()
        .map(|entry| UsageApplicationDuration {
            app_id: entry.app_id,
            bucket_key: entry.bucket_key,
            timezone_id: entry.timezone_id,
            utc_offset_seconds: entry.utc_offset_seconds,
            duration_ns: entry.duration_ns,
            last_wall_utc_ms: entry.last_wall_utc_ms,
        })
        .collect()
}

fn summary_status(
    runtime: &UsageRuntimeState,
    coverage: &UsageCoverageState,
    truncated: bool,
    bucket_coverage: BucketCoverage,
) -> (CapabilityAvailability, &'static str, bool) {
    if truncated {
        return (
            CapabilityAvailability::Degraded,
            "usage_summary_truncated",
            false,
        );
    }
    if coverage.event_gap_count > 0 {
        return (
            CapabilityAvailability::Degraded,
            "usage_historical_gaps_present",
            false,
        );
    }
    match bucket_coverage {
        BucketCoverage::NotStarted => {
            return (
                CapabilityAvailability::Degraded,
                "usage_tracking_not_started_for_bucket",
                false,
            );
        }
        BucketCoverage::Partial => {
            return (
                CapabilityAvailability::Degraded,
                "usage_tracking_epoch_partial",
                false,
            );
        }
        BucketCoverage::Unknown => {
            return (
                CapabilityAvailability::Degraded,
                "usage_tracking_epoch_unknown",
                true,
            );
        }
        BucketCoverage::Complete => {}
    }
    (
        runtime.capability.status,
        stable_runtime_reason(&runtime.capability.reason),
        matches!(
            runtime.capability.status,
            CapabilityAvailability::Degraded | CapabilityAvailability::Unreachable
        ),
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BucketCoverage {
    Complete,
    Partial,
    NotStarted,
    Unknown,
}

fn bucket_coverage(query: &UsageSummaryQuery, coverage: &UsageCoverageState) -> BucketCoverage {
    let start_key = match query.period {
        UsagePeriod::Daily => coverage.tracking_start_daily_key.as_deref(),
        UsagePeriod::Weekly => coverage.tracking_start_weekly_key.as_deref(),
    };
    let Some(start_key) = start_key else {
        return BucketCoverage::Unknown;
    };
    match query.bucket_key.as_str().cmp(start_key) {
        std::cmp::Ordering::Less => BucketCoverage::NotStarted,
        std::cmp::Ordering::Equal => BucketCoverage::Partial,
        std::cmp::Ordering::Greater => BucketCoverage::Complete,
    }
}

fn publish_current_state(
    worker: &WorkerState,
    shared: &Arc<RwLock<UsageRuntimeState>>,
    reason: &'static str,
) {
    let coverage = worker.tracker.store().coverage_state().unwrap_or_default();
    publish_worker_state(shared, worker, &coverage, reason);
}

fn publish_worker_state(
    shared: &Arc<RwLock<UsageRuntimeState>>,
    worker: &WorkerState,
    coverage: &UsageCoverageState,
    reason: &'static str,
) {
    let healthy =
        worker.niri_snapshot_seen && worker.session_available && worker.wayland_idle.is_some();
    if let Ok(mut state) = shared.write() {
        state.capability = if healthy {
            CapabilityRuntimeState::healthy("usage_tracking_active")
        } else {
            CapabilityRuntimeState::degraded(reason)
        };
        state.niri_connected = worker.niri_snapshot_seen;
        state.session_available = worker.session_available;
        state.last_checkpoint_unix_ms = worker.last_checkpoint_unix_ms;
        state.event_gap_count = coverage.event_gap_count;
    }
}

fn publish_unavailable(shared: &Arc<RwLock<UsageRuntimeState>>, reason: &'static str) {
    if let Ok(mut state) = shared.write() {
        state.capability = CapabilityRuntimeState::unreachable(reason);
        state.niri_connected = false;
        state.session_available = false;
    }
}

fn publish_unsupported(shared: &Arc<RwLock<UsageRuntimeState>>, reason: &'static str) {
    if let Ok(mut state) = shared.write() {
        state.capability = CapabilityRuntimeState::unsupported(reason);
        state.niri_connected = false;
        state.session_available = false;
    }
}

async fn wait_for_shutdown(shutdown: &mut watch::Receiver<bool>) {
    while !*shutdown.borrow() {
        if shutdown.changed().await.is_err() {
            break;
        }
    }
}

fn usage_database_path_from_environment() -> Result<PathBuf, UsageRuntimeError> {
    let base = if let Some(path) = std::env::var_os("XDG_STATE_HOME") {
        PathBuf::from(path)
    } else {
        let home = std::env::var_os("HOME").ok_or_else(|| {
            UsageRuntimeError::new(
                "usage_state_unavailable",
                "home_directory_unavailable",
                false,
            )
        })?;
        PathBuf::from(home).join(".local/state")
    };
    prepare_database_path(&base, Uid::effective().as_raw())
}

fn prepare_database_path(base: &Path, expected_uid: u32) -> Result<PathBuf, UsageRuntimeError> {
    if !base.is_absolute() {
        return Err(UsageRuntimeError::new(
            "usage_state_unsafe",
            "usage_state_path_not_absolute",
            false,
        ));
    }
    validate_private_directory(base, expected_uid, "usage_state_directory_unsafe")?;
    let directory = base.join(STATE_DIRECTORY);
    if !directory.exists() {
        DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(|_| {
                UsageRuntimeError::new(
                    "usage_state_unavailable",
                    "usage_state_directory_create_failed",
                    true,
                )
            })?;
    }
    validate_private_directory(&directory, expected_uid, "usage_state_directory_unsafe")?;
    let database = directory.join(DATABASE_FILE);
    match fs::symlink_metadata(&database) {
        Ok(metadata) => validate_database_file(&metadata, expected_uid)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&database)
                .map_err(|_| {
                    UsageRuntimeError::new(
                        "usage_state_unavailable",
                        "usage_database_create_failed",
                        true,
                    )
                })?;
        }
        Err(_) => {
            return Err(UsageRuntimeError::new(
                "usage_state_unavailable",
                "usage_database_metadata_failed",
                true,
            ));
        }
    }
    Ok(database)
}

fn validate_private_directory(
    path: &Path,
    expected_uid: u32,
    reason: &'static str,
) -> Result<(), UsageRuntimeError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| UsageRuntimeError::new("usage_state_unavailable", reason, true))?;
    if metadata.file_type().is_symlink()
        || !metadata.file_type().is_dir()
        || metadata.uid() != expected_uid
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(UsageRuntimeError::new("usage_state_unsafe", reason, false));
    }
    Ok(())
}

fn validate_database_file(
    metadata: &fs::Metadata,
    expected_uid: u32,
) -> Result<(), UsageRuntimeError> {
    if metadata.file_type().is_symlink()
        || !metadata.file_type().is_file()
        || metadata.uid() != expected_uid
        || metadata.permissions().mode() & 0o777 != 0o600
    {
        return Err(UsageRuntimeError::new(
            "usage_state_unsafe",
            "usage_database_unsafe",
            false,
        ));
    }
    Ok(())
}

fn stable_runtime_reason(reason: &str) -> &'static str {
    match reason {
        "usage_tracking_active" => "usage_tracking_active",
        "usage_warming_up" => "usage_warming_up",
        "usage_waiting_for_niri_snapshot" => "usage_waiting_for_niri_snapshot",
        "niri_event_stream_unavailable" => "niri_event_stream_unavailable",
        "niri_event_stream_disconnected" => "niri_event_stream_disconnected",
        "wayland_idle_event_stream_unavailable" => "wayland_idle_event_stream_unavailable",
        "wayland_idle_event_stream_disconnected" => "wayland_idle_event_stream_disconnected",
        "logind_session_unavailable" => "logind_session_unavailable",
        "logind_probe_unavailable" => "logind_probe_unavailable",
        "logind_event_stream_unavailable" => "logind_event_stream_unavailable",
        "logind_event_stream_disconnected" => "logind_event_stream_disconnected",
        "logind_session_state_changed" => "logind_session_state_changed",
        "usage_database_unavailable" => "usage_database_unavailable",
        "usage_database_corrupt" => "usage_database_corrupt",
        "usage_database_schema_unsupported" => "usage_database_schema_unsupported",
        "usage_migration_backup_path_invalid" => "usage_migration_backup_path_invalid",
        "usage_migration_backup_snapshot_failed" => "usage_migration_backup_snapshot_failed",
        "usage_migration_backup_metadata_failed" => "usage_migration_backup_metadata_failed",
        "usage_migration_backup_permissions_failed" => "usage_migration_backup_permissions_failed",
        "usage_migration_backup_sync_failed" => "usage_migration_backup_sync_failed",
        "usage_migration_backup_publish_failed" => "usage_migration_backup_publish_failed",
        "usage_migration_backup_unsafe" => "usage_migration_backup_unsafe",
        "usage_migration_backup_invalid" => "usage_migration_backup_invalid",
        "usage_state_directory_unsafe" => "usage_state_directory_unsafe",
        "usage_database_unsafe" => "usage_database_unsafe",
        "usage_store_poisoned" => "usage_store_poisoned",
        "usage_shutting_down" => "usage_shutting_down",
        _ => "usage_runtime_unavailable",
    }
}

fn usage_store_open_reason(error: &localdesk_usage::UsageStoreError) -> &'static str {
    match error {
        localdesk_usage::UsageStoreError::Corrupt => "usage_database_corrupt",
        localdesk_usage::UsageStoreError::UnsupportedSchema { .. } => {
            "usage_database_schema_unsupported"
        }
        localdesk_usage::UsageStoreError::MigrationBackup { reason } => reason,
        _ => "usage_database_unavailable",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use localdesk_usage::{ClockSample, SegmentedAggregateEntry, SegmentedAggregateQuery};

    fn query_client_fixture() -> (tempfile::TempDir, QueryClient, mpsc::Receiver<QueryCommand>) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.sqlite3");
        let recovered_at = ClockSample {
            boot_id: "test-boot".into(),
            monotonic_ns: 0,
            wall_utc_ms: 1_767_225_600_000,
            utc_offset_seconds: 0,
            timezone_id: "UTC".into(),
        };
        let store = UsageStore::open(&path, &recovered_at).unwrap();
        let reader = UsageReader::open(&path).unwrap();
        let control = Arc::new(QueryControl {
            current_token: Mutex::new(None),
            interrupt: reader.interrupt_handle(),
            cancelled_token: AtomicU64::new(0),
            stopping: AtomicBool::new(false),
        });
        let (sender, receiver) = mpsc::sync_channel(1);
        let client = QueryClient {
            sender,
            busy: Arc::new(AtomicBool::new(false)),
            next_token: Arc::new(AtomicU64::new(1)),
            control,
        };
        drop(store);
        (directory, client, receiver)
    }

    fn daily_query() -> UsageSummaryQuery {
        UsageSummaryQuery {
            period: UsagePeriod::Daily,
            bucket_key: "2026-01-01".into(),
        }
    }

    #[test]
    fn state_path_is_private_and_rejects_symlink_database() {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let legacy_path = directory.path().join("localdesk/usage.sqlite3");
        fs::create_dir(directory.path().join("localdesk")).unwrap();
        fs::set_permissions(
            directory.path().join("localdesk"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        fs::write(&legacy_path, b"legacy-usage-data").unwrap();
        fs::set_permissions(&legacy_path, fs::Permissions::from_mode(0o600)).unwrap();

        let path = prepare_database_path(directory.path(), Uid::effective().as_raw()).unwrap();
        assert_eq!(path.file_name().unwrap(), DATABASE_FILE);
        assert_eq!(fs::read(&legacy_path).unwrap(), b"legacy-usage-data");
        let metadata = fs::symlink_metadata(&path).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);

        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("target", &path).unwrap();
        assert_eq!(
            prepare_database_path(directory.path(), Uid::effective().as_raw())
                .unwrap_err()
                .reason,
            "usage_database_unsafe"
        );
    }

    #[test]
    fn usage_store_open_errors_preserve_corrupt_and_schema_reasons() {
        assert_eq!(
            usage_store_open_reason(&localdesk_usage::UsageStoreError::Corrupt),
            "usage_database_corrupt"
        );
        assert_eq!(
            usage_store_open_reason(&localdesk_usage::UsageStoreError::UnsupportedSchema {
                found: 99,
                supported: 2,
            }),
            "usage_database_schema_unsupported"
        );
        assert_eq!(
            stable_runtime_reason(usage_store_open_reason(
                &localdesk_usage::UsageStoreError::MigrationBackup {
                    reason: "usage_migration_backup_invalid",
                }
            )),
            "usage_migration_backup_invalid"
        );
        assert_eq!(
            usage_store_open_reason(&localdesk_usage::UsageStoreError::InvalidUsageEpoch),
            "usage_database_unavailable"
        );
    }

    #[test]
    fn projection_keeps_timezone_segments_separate() {
        let projected = project_aggregates(SegmentedAggregateQuery {
            truncated: false,
            entries: vec![
                SegmentedAggregateEntry {
                    app_id: "editor".into(),
                    bucket_key: "2026-08-09".into(),
                    timezone_id: "UTC".into(),
                    utc_offset_seconds: 0,
                    bucket_start_utc_ms: 0,
                    last_wall_utc_ms: 1,
                    duration_ns: 2,
                },
                SegmentedAggregateEntry {
                    app_id: "editor".into(),
                    bucket_key: "2026-08-09".into(),
                    timezone_id: "Asia/Shanghai".into(),
                    utc_offset_seconds: 28_800,
                    bucket_start_utc_ms: 0,
                    last_wall_utc_ms: 2,
                    duration_ns: 3,
                },
            ],
        });
        assert_eq!(projected.len(), 2);
        assert_ne!(projected[0].timezone_id, projected[1].timezone_id);
    }

    #[test]
    fn epoch_marks_prior_current_and_later_buckets_truthfully() {
        let coverage = UsageCoverageState {
            tracking_started_wall_utc_ms: Some(1_786_503_864_075),
            tracking_start_daily_key: Some("2026-08-12".into()),
            tracking_start_weekly_key: Some("2026-W33".into()),
            ..UsageCoverageState::default()
        };
        let mut query = daily_query();
        query.bucket_key = "2026-08-11".into();
        assert_eq!(
            bucket_coverage(&query, &coverage),
            BucketCoverage::NotStarted
        );
        query.bucket_key = "2026-08-12".into();
        assert_eq!(bucket_coverage(&query, &coverage), BucketCoverage::Partial);
        query.bucket_key = "2026-08-13".into();
        assert_eq!(bucket_coverage(&query, &coverage), BucketCoverage::Complete);

        query.period = UsagePeriod::Weekly;
        query.bucket_key = "2026-W33".into();
        assert_eq!(bucket_coverage(&query, &coverage), BucketCoverage::Partial);
        query.bucket_key = "2026-W34".into();
        assert_eq!(bucket_coverage(&query, &coverage), BucketCoverage::Complete);

        let runtime = UsageRuntimeState {
            capability: CapabilityRuntimeState::healthy("usage_tracking_active"),
            niri_connected: true,
            session_available: true,
            last_checkpoint_unix_ms: Some(1_786_503_870_000),
            event_gap_count: 0,
            writer_client: None,
            query_client: None,
        };
        assert_eq!(
            summary_status(&runtime, &coverage, false, BucketCoverage::Partial),
            (
                CapabilityAvailability::Degraded,
                "usage_tracking_epoch_partial",
                false,
            )
        );
        assert_eq!(
            summary_status(&runtime, &coverage, false, BucketCoverage::Unknown),
            (
                CapabilityAvailability::Degraded,
                "usage_tracking_epoch_unknown",
                true,
            )
        );
    }

    #[test]
    fn only_current_bucket_requires_a_fresh_writer_checkpoint() {
        let mut clock = SystemClock;
        let sample = clock.sample().unwrap();
        for (period, kind) in [
            (UsagePeriod::Daily, SummaryKind::Daily),
            (UsagePeriod::Weekly, SummaryKind::Weekly),
        ] {
            let current = SummaryBucket::for_sample(kind, &sample).unwrap();
            let current_query = UsageSummaryQuery {
                period,
                bucket_key: current.bucket_key,
            };
            assert!(query_targets_current_bucket(&current_query).unwrap());
        }

        assert!(
            !query_targets_current_bucket(&UsageSummaryQuery {
                period: UsagePeriod::Daily,
                bucket_key: "2026-01-01".into(),
            })
            .unwrap()
        );
        assert!(
            !query_targets_current_bucket(&UsageSummaryQuery {
                period: UsagePeriod::Weekly,
                bucket_key: "2026-W01".into(),
            })
            .unwrap()
        );
    }

    #[test]
    fn query_client_rejects_concurrent_work_without_queue_growth() {
        let (_directory, client, _receiver) = query_client_fixture();
        let (first_reply, _) = oneshot::channel();
        let first_token = client.try_send(daily_query(), first_reply).unwrap();

        let (second_reply, _) = oneshot::channel();
        let error = client.try_send(daily_query(), second_reply).unwrap_err();
        assert_eq!(error.reason, "usage_query_busy");

        client.cancel(first_token);
        assert!(!client.control.begin(first_token));
        assert!(client.control.begin(first_token + 1));
        client.control.cancel(first_token);
        client.control.finish(first_token + 1);
    }

    #[tokio::test]
    async fn checkpoint_wait_honors_the_request_deadline() {
        let (writer, _receiver) = mpsc::channel();
        let started = tokio::time::Instant::now();
        let error = checkpoint_writer(Some(writer), started + Duration::from_millis(20))
            .await
            .unwrap_err();
        assert_eq!(error.reason, "usage_checkpoint_timeout");
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
