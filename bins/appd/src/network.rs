use localdesk_domain::{
    CapabilityAvailability, CapabilityRuntimeState, NETWORK_TOTAL_SCOPE, NetworkApplicationTraffic,
    NetworkByteTotals, NetworkCapabilityState, NetworkCoverage, NetworkFreshness,
    NetworkInterfaceKind, NetworkInterfaceSample, NetworkInterfaceTransition,
    NetworkLayeredAccounting, NetworkRate, NetworkRateState, NetworkSnapshot, NetworkTrafficTotals,
};
use localdesk_network::{
    CapabilityState as SourceCapabilityState, CapabilityStatus, CgroupTraffic, CollectError,
    InterfaceKind, InterfaceTransition, LayeredAccounting, NetworkMonitor, PerAppCollector,
    PerAppCollectorError, RateState, TrafficRate,
};
use localdesk_network_helper_protocol::{
    CapabilityReason as HelperCapabilityReason, CapabilityStatus as HelperCapabilityStatus,
    CgroupBinding, CollectionReplyBody, CollectionRequest, FrameError as HelperFrameError,
    HelperErrorCode, MAX_FRAME_BYTES as MAX_HELPER_FRAME_BYTES, decode_reply, encode_request,
};
use localdesk_telemetry::TelemetryManagerHandle;
use std::{
    collections::{HashMap, HashSet},
    fs, io,
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, PermissionsExt},
    },
    path::{Component, Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{Arc, RwLock, mpsc},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{oneshot, watch},
    time::MissedTickBehavior,
};

const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const FRESH_FOR: Duration = Duration::from_secs(3);
const RETAIN_STALE_FOR: Duration = Duration::from_secs(10);
const NETWORK_HELPER_NAME: &str = "localdesk-network-helper";
const NETWORK_CGROUP_ROOT: &str = "/sys/fs/cgroup";
const NETWORK_HELPER_DEADLINE: Duration = Duration::from_millis(500);

pub struct NetworkHelperCollector {
    telemetry: TelemetryManagerHandle,
    helper_path: Result<PathBuf, &'static str>,
    session: Option<NetworkHelperSession>,
    capability: SourceCapabilityState,
    generation: u64,
}

impl NetworkHelperCollector {
    pub fn new(telemetry: TelemetryManagerHandle) -> Self {
        Self::with_helper_path(telemetry, production_network_helper_path())
    }

    fn with_helper_path(
        telemetry: TelemetryManagerHandle,
        helper_path: Result<PathBuf, &'static str>,
    ) -> Self {
        let capability = match &helper_path {
            Ok(path) => match validate_helper_path(path) {
                Ok(()) => SourceCapabilityState::degraded("network_helper_not_started"),
                Err(reason) => SourceCapabilityState::unsupported(reason),
            },
            Err(reason) => SourceCapabilityState::unsupported(reason),
        };
        Self {
            telemetry,
            helper_path,
            session: None,
            capability,
            generation: 0,
        }
    }

    fn bindings(&self) -> Result<Vec<CgroupBinding>, PerAppCollectorError> {
        let snapshot = self
            .telemetry
            .cgroup_bindings()
            .map_err(|_| PerAppCollectorError::new("network_identity_binding_store_unavailable"))?;
        if !snapshot.available {
            return Err(PerAppCollectorError::new(snapshot.reason));
        }
        let mut ids = HashSet::with_capacity(snapshot.bindings.len());
        let mut bindings = Vec::with_capacity(snapshot.bindings.len());
        for binding in snapshot.bindings {
            let path = cgroup_path_on_disk(&binding.cgroup_path)?;
            let metadata = fs::metadata(path)
                .map_err(|_| PerAppCollectorError::new("network_cgroup_binding_unavailable"))?;
            if !metadata.is_dir() {
                return Err(PerAppCollectorError::new(
                    "network_cgroup_binding_not_directory",
                ));
            }
            let cgroup_id = metadata.ino();
            if cgroup_id == 0 || !ids.insert(cgroup_id) {
                return Err(PerAppCollectorError::new(
                    "network_cgroup_binding_identity_invalid",
                ));
            }
            bindings.push(CgroupBinding {
                cgroup_id,
                application_key: binding.application_key,
            });
        }
        bindings.sort_by_key(|binding| binding.cgroup_id);
        Ok(bindings)
    }

    fn ensure_session(&mut self) -> Result<&mut NetworkHelperSession, PerAppCollectorError> {
        if self.session.is_none() {
            let path = self
                .helper_path
                .as_ref()
                .map_err(|reason| PerAppCollectorError::new(reason))?;
            validate_helper_path(path).map_err(PerAppCollectorError::new)?;
            self.session =
                Some(NetworkHelperSession::spawn(path).map_err(PerAppCollectorError::new)?);
        }
        self.session
            .as_mut()
            .ok_or_else(|| PerAppCollectorError::new("network_helper_session_unavailable"))
    }

    fn fail(&mut self, reason: &'static str) -> PerAppCollectorError {
        self.session.take();
        self.capability = if matches!(
            reason,
            "network_helper_missing"
                | "network_helper_unexecutable"
                | "network_helper_path_rejected"
        ) {
            SourceCapabilityState::unsupported(reason)
        } else {
            SourceCapabilityState::degraded(reason)
        };
        PerAppCollectorError::new(reason)
    }
}

impl PerAppCollector for NetworkHelperCollector {
    fn capability(&self) -> SourceCapabilityState {
        self.capability.clone()
    }

    fn collect(&mut self) -> Result<Vec<CgroupTraffic>, PerAppCollectorError> {
        let bindings = match self.bindings() {
            Ok(bindings) => bindings,
            Err(error) => return Err(self.fail(error.reason)),
        };
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| self.fail("network_helper_generation_exhausted"))?;
        let generation = self.generation;
        let application_keys = bindings
            .iter()
            .map(|binding| (binding.cgroup_id, binding.application_key.clone()))
            .collect::<HashMap<_, _>>();
        let request = CollectionRequest::collect(generation, bindings);
        let reply = match self.ensure_session() {
            Ok(session) => session.exchange(&request),
            Err(error) => return Err(self.fail(error.reason)),
        };
        let reply = match reply {
            Ok(reply) if reply.generation == generation => reply,
            Ok(_) => return Err(self.fail("network_helper_generation_mismatch")),
            Err(reason) => return Err(self.fail(reason)),
        };
        match reply.body {
            CollectionReplyBody::Error(error) => Err(self.fail(helper_error_reason(error.code))),
            CollectionReplyBody::Snapshot(snapshot) => {
                self.capability = helper_capability(snapshot.capability);
                let mut records = Vec::with_capacity(snapshot.records.len());
                for record in snapshot.records {
                    let Some(application_key) = application_keys.get(&record.cgroup_id) else {
                        return Err(self.fail("network_helper_returned_unrequested_cgroup"));
                    };
                    records.push(CgroupTraffic {
                        cgroup_id: record.cgroup_id,
                        application_key: application_key.clone(),
                        rx_bytes: record.rx_bytes,
                        tx_bytes: record.tx_bytes,
                    });
                }
                Ok(records)
            }
        }
    }
}

struct NetworkHelperSession {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
}

impl NetworkHelperSession {
    fn spawn(path: &Path) -> Result<Self, &'static str> {
        let mut child = Command::new(path)
            .arg("--cgroup-root")
            .arg(NETWORK_CGROUP_ROOT)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| match error.kind() {
                io::ErrorKind::NotFound => "network_helper_missing",
                io::ErrorKind::PermissionDenied => "network_helper_unexecutable",
                _ => "network_helper_spawn_failed",
            })?;
        let Some(stdin) = child.stdin.take() else {
            terminate_child(&mut child);
            return Err("network_helper_stdio_unavailable");
        };
        let Some(stdout) = child.stdout.take() else {
            terminate_child(&mut child);
            return Err("network_helper_stdio_unavailable");
        };
        set_nonblocking(stdin.as_raw_fd()).map_err(|_| "network_helper_stdio_unavailable")?;
        set_nonblocking(stdout.as_raw_fd()).map_err(|_| "network_helper_stdio_unavailable")?;
        Ok(Self {
            child,
            stdin,
            stdout,
        })
    }

    fn exchange(
        &mut self,
        request: &CollectionRequest,
    ) -> Result<localdesk_network_helper_protocol::CollectionReply, &'static str> {
        let payload = encode_request(request).map_err(helper_frame_reason)?;
        let deadline = Instant::now() + NETWORK_HELPER_DEADLINE;
        write_helper_frame(self.stdin.as_raw_fd(), &payload, deadline)?;
        let payload = read_helper_frame(self.stdout.as_raw_fd(), deadline)?;
        decode_reply(&payload).map_err(helper_frame_reason)
    }
}

impl Drop for NetworkHelperSession {
    fn drop(&mut self) {
        terminate_child(&mut self.child);
    }
}

fn terminate_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn production_network_helper_path() -> Result<PathBuf, &'static str> {
    let executable = std::env::current_exe().map_err(|_| "network_helper_path_unavailable")?;
    let parent = executable
        .parent()
        .ok_or("network_helper_path_unavailable")?;
    Ok(parent.join(NETWORK_HELPER_NAME))
}

fn validate_helper_path(path: &Path) -> Result<(), &'static str> {
    let metadata = fs::symlink_metadata(path).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => "network_helper_missing",
        io::ErrorKind::PermissionDenied => "network_helper_unexecutable",
        _ => "network_helper_path_rejected",
    })?;
    if metadata.file_type().is_symlink() {
        return Err("network_helper_path_rejected");
    }
    if !metadata.file_type().is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err("network_helper_unexecutable");
    }
    if metadata.permissions().mode() & 0o022 != 0 {
        return Err("network_helper_path_rejected");
    }
    let executable = std::env::current_exe().map_err(|_| "network_helper_path_unavailable")?;
    let executable_metadata =
        fs::metadata(executable).map_err(|_| "network_helper_path_unavailable")?;
    if metadata.uid() != executable_metadata.uid() {
        return Err("network_helper_path_rejected");
    }
    Ok(())
}

fn cgroup_path_on_disk(cgroup_path: &str) -> Result<PathBuf, PerAppCollectorError> {
    let mut result = PathBuf::from("/sys/fs/cgroup");
    for component in Path::new(cgroup_path).components() {
        match component {
            Component::RootDir => {}
            Component::Normal(component) => result.push(component),
            _ => {
                return Err(PerAppCollectorError::new(
                    "network_cgroup_binding_path_invalid",
                ));
            }
        }
    }
    Ok(result)
}

fn helper_capability(
    capability: localdesk_network_helper_protocol::HelperCapability,
) -> SourceCapabilityState {
    let reason = helper_capability_reason(capability.reason);
    match capability.status {
        HelperCapabilityStatus::Healthy => SourceCapabilityState::healthy(reason),
        HelperCapabilityStatus::Degraded => SourceCapabilityState::degraded(reason),
        HelperCapabilityStatus::Unsupported => SourceCapabilityState::unsupported(reason),
    }
}

fn helper_capability_reason(reason: HelperCapabilityReason) -> &'static str {
    match reason {
        HelperCapabilityReason::CoreCgroupCollectorAttached => "core_cgroup_collector_attached",
        HelperCapabilityReason::CoreCgroupCollectorNotAttached => {
            "core_cgroup_collector_not_attached"
        }
        HelperCapabilityReason::CoreCgroupCollectorNotBuilt => "core_cgroup_collector_not_built",
        HelperCapabilityReason::UnprivilegedBpfPermanentlyDisabled => {
            "unprivileged_bpf_permanently_disabled"
        }
        HelperCapabilityReason::KernelBtfUnavailable => "kernel_btf_unavailable",
        HelperCapabilityReason::LibbpfRuntimeUnavailable => "libbpf_runtime_unavailable",
        HelperCapabilityReason::HelperPermissionDenied => "network_helper_permission_denied",
        HelperCapabilityReason::IdentityBindingsUnavailable => {
            "network_identity_bindings_unavailable"
        }
    }
}

fn helper_error_reason(code: HelperErrorCode) -> &'static str {
    match code {
        HelperErrorCode::MalformedRequest
        | HelperErrorCode::UnsupportedVersion
        | HelperErrorCode::OversizedFrame
        | HelperErrorCode::InvalidRequest => "network_helper_protocol_error",
        HelperErrorCode::CollectorUnavailable => "network_helper_collector_unavailable",
        HelperErrorCode::PermissionDenied => "network_helper_permission_denied",
        HelperErrorCode::LimitExceeded => "network_helper_limit_exceeded",
        HelperErrorCode::Internal => "network_helper_internal_error",
    }
}

fn helper_frame_reason(error: HelperFrameError) -> &'static str {
    match error {
        HelperFrameError::Oversized { .. } => "network_helper_frame_oversized",
        HelperFrameError::UnsupportedVersion(_)
        | HelperFrameError::MalformedJson(_)
        | HelperFrameError::InvalidPayload(_)
        | HelperFrameError::Empty
        | HelperFrameError::LengthOverflow => "network_helper_protocol_error",
        HelperFrameError::Io(_)
        | HelperFrameError::TruncatedLength
        | HelperFrameError::TruncatedPayload => "network_helper_io_failed",
    }
}

fn set_nonblocking(fd: libc::c_int) -> io::Result<()> {
    // SAFETY: fcntl only reads descriptor flags and does not retain pointers.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd remains owned by the ChildStdin/ChildStdout value.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn write_helper_frame(
    fd: libc::c_int,
    payload: &[u8],
    deadline: Instant,
) -> Result<(), &'static str> {
    if payload.is_empty() || payload.len() > MAX_HELPER_FRAME_BYTES {
        return Err("network_helper_frame_oversized");
    }
    let length = u32::try_from(payload.len()).map_err(|_| "network_helper_frame_oversized")?;
    write_fd_all(fd, &length.to_be_bytes(), deadline)?;
    write_fd_all(fd, payload, deadline)
}

fn read_helper_frame(fd: libc::c_int, deadline: Instant) -> Result<Vec<u8>, &'static str> {
    let mut prefix = [0_u8; 4];
    read_fd_exact(fd, &mut prefix, deadline)?;
    let length = u32::from_be_bytes(prefix) as usize;
    if length == 0 || length > MAX_HELPER_FRAME_BYTES {
        return Err("network_helper_frame_oversized");
    }
    let mut payload = vec![0_u8; length];
    read_fd_exact(fd, &mut payload, deadline)?;
    Ok(payload)
}

fn write_fd_all(fd: libc::c_int, mut bytes: &[u8], deadline: Instant) -> Result<(), &'static str> {
    while !bytes.is_empty() {
        wait_for_fd(fd, libc::POLLOUT, deadline)?;
        // SAFETY: bytes is readable for bytes.len(); write does not retain it.
        let written = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if written > 0 {
            bytes = &bytes[written as usize..];
            continue;
        }
        if written == 0 {
            return Err("network_helper_io_failed");
        }
        let error = io::Error::last_os_error();
        if matches!(
            error.kind(),
            io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
        ) {
            continue;
        }
        return Err("network_helper_io_failed");
    }
    Ok(())
}

fn read_fd_exact(
    fd: libc::c_int,
    mut bytes: &mut [u8],
    deadline: Instant,
) -> Result<(), &'static str> {
    while !bytes.is_empty() {
        wait_for_fd(fd, libc::POLLIN, deadline)?;
        // SAFETY: bytes is writable for bytes.len(); read does not retain it.
        let read = unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len()) };
        if read > 0 {
            bytes = &mut bytes[read as usize..];
            continue;
        }
        if read == 0 {
            return Err("network_helper_eof");
        }
        let error = io::Error::last_os_error();
        if matches!(
            error.kind(),
            io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
        ) {
            continue;
        }
        return Err("network_helper_io_failed");
    }
    Ok(())
}

fn wait_for_fd(
    fd: libc::c_int,
    events: libc::c_short,
    deadline: Instant,
) -> Result<(), &'static str> {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err("network_helper_timeout");
        }
        let remaining = deadline.saturating_duration_since(now);
        let timeout_ms = remaining.as_millis().max(1).min(i32::MAX as u128) as i32;
        let mut poll_fd = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        // SAFETY: poll_fd points to one writable pollfd for the duration of the call.
        let result = unsafe { libc::poll(&mut poll_fd, 1, timeout_ms) };
        if result > 0 {
            if poll_fd.revents & events != 0 {
                return Ok(());
            }
            if poll_fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                return Err("network_helper_io_failed");
            }
            continue;
        }
        if result == 0 {
            return Err("network_helper_timeout");
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return Err("network_helper_io_failed");
    }
}

#[derive(Clone)]
pub struct NetworkHandle {
    state: Arc<RwLock<StoredSnapshot>>,
}

struct StoredSnapshot {
    snapshot: NetworkSnapshot,
    sampled_at: Option<Instant>,
    last_failure_reason: Option<String>,
}

impl NetworkHandle {
    pub fn snapshot(&self) -> NetworkSnapshot {
        let Ok(state) = self.state.read() else {
            return NetworkSnapshot::unavailable("network_store_poisoned");
        };
        snapshot_with_current_freshness(&state, Instant::now())
    }

    pub fn capability_states(&self) -> (CapabilityRuntimeState, CapabilityRuntimeState) {
        let snapshot = self.snapshot();
        (
            capability_state(&snapshot.system_traffic),
            capability_state(&snapshot.per_application),
        )
    }
}

pub struct NetworkSupervisor {
    monitor: Option<NetworkMonitor>,
    state: Arc<RwLock<StoredSnapshot>>,
    previous_boottime_ms: Option<u64>,
}

impl NetworkSupervisor {
    pub fn new(monitor: NetworkMonitor) -> Self {
        Self {
            monitor: Some(monitor),
            state: Arc::new(RwLock::new(StoredSnapshot {
                snapshot: NetworkSnapshot::unavailable("network_warming_up"),
                sampled_at: None,
                last_failure_reason: None,
            })),
            previous_boottime_ms: None,
        }
    }

    pub fn handle(&self) -> NetworkHandle {
        NetworkHandle {
            state: Arc::clone(&self.state),
        }
    }

    pub async fn run(mut self, mut shutdown: watch::Receiver<bool>) {
        let Some(monitor) = self.monitor.take() else {
            self.publish_failure("network_monitor_unavailable");
            return;
        };
        let Ok((worker_tx, worker)) = spawn_network_worker(monitor) else {
            self.publish_failure("network_worker_start_failed");
            return;
        };
        let mut interval = tokio::time::interval(SAMPLE_INTERVAL);
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        'running: loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break 'running;
                    }
                }
                _ = interval.tick() => {
                    match self.collect_once(&worker_tx, &mut shutdown).await {
                        CollectControl::Continue => {}
                        CollectControl::Shutdown | CollectControl::WorkerFailed => break 'running,
                    }
                }
            }
        }
        let _ = worker_tx.send(WorkerCommand::Shutdown);
        if !join_network_worker(worker).await {
            self.publish_failure("network_worker_task_failed");
        }
    }

    async fn collect_once(
        &mut self,
        worker_tx: &mpsc::Sender<WorkerCommand>,
        shutdown: &mut watch::Receiver<bool>,
    ) -> CollectControl {
        let (reply_tx, mut reply_rx) = oneshot::channel();
        if worker_tx.send(WorkerCommand::Collect(reply_tx)).is_err() {
            self.publish_failure("network_worker_unavailable");
            return CollectControl::WorkerFailed;
        }
        loop {
            tokio::select! {
                result = &mut reply_rx => {
                    let Ok(result) = result else {
                        self.publish_failure("network_worker_task_failed");
                        return CollectControl::WorkerFailed;
                    };
                    match result {
                        Ok(snapshot) => self.publish_success(snapshot),
                        Err(error) => self.publish_failure(collect_error_reason(&error)),
                    }
                    return CollectControl::Continue;
                }
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return CollectControl::Shutdown;
                    }
                }
            }
        }
    }

    fn publish_success(&mut self, snapshot: localdesk_network::NetworkSnapshot) {
        let captured_at_unix_ms = unix_time_ms();
        let observed_boottime_ms =
            u64::try_from(snapshot.observed_boottime.as_millis()).unwrap_or(u64::MAX);
        let sample_interval_ms = self
            .previous_boottime_ms
            .and_then(|previous| observed_boottime_ms.checked_sub(previous));
        self.previous_boottime_ms = Some(observed_boottime_ms);
        let public = project_snapshot(
            snapshot,
            captured_at_unix_ms,
            observed_boottime_ms,
            sample_interval_ms,
        );
        if public.validate().is_err() {
            self.publish_failure("network_snapshot_invalid");
            return;
        }
        if let Ok(mut state) = self.state.write() {
            *state = StoredSnapshot {
                snapshot: public,
                sampled_at: Some(Instant::now()),
                last_failure_reason: None,
            };
        }
    }

    fn publish_failure(&self, reason: &'static str) {
        let Ok(mut state) = self.state.write() else {
            return;
        };
        if state.sampled_at.is_none() {
            state.snapshot = NetworkSnapshot::unavailable(reason);
            state.last_failure_reason = Some(reason.to_owned());
            return;
        }
        state.snapshot.freshness = NetworkFreshness::Stale;
        state.snapshot.system_traffic = NetworkCapabilityState {
            status: CapabilityAvailability::Degraded,
            reason: reason.to_owned(),
        };
        degrade_supported_per_app(&mut state.snapshot.per_application, reason);
        state.snapshot.retryable = true;
        state.last_failure_reason = Some(reason.to_owned());
    }
}

enum WorkerCommand {
    Collect(oneshot::Sender<Result<localdesk_network::NetworkSnapshot, CollectError>>),
    Shutdown,
}

enum CollectControl {
    Continue,
    Shutdown,
    WorkerFailed,
}

fn spawn_network_worker(
    mut monitor: NetworkMonitor,
) -> Result<(mpsc::Sender<WorkerCommand>, thread::JoinHandle<()>), std::io::Error> {
    let (sender, receiver) = mpsc::channel();
    let worker = thread::Builder::new()
        .name("localdesk-network".to_owned())
        .spawn(move || {
            while let Ok(command) = receiver.recv() {
                match command {
                    WorkerCommand::Collect(reply) => {
                        let _ = reply.send(monitor.collect());
                    }
                    WorkerCommand::Shutdown => break,
                }
            }
        })?;
    Ok((sender, worker))
}

async fn join_network_worker(worker: thread::JoinHandle<()>) -> bool {
    while !worker.is_finished() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    worker.join().is_ok()
}

fn snapshot_with_current_freshness(state: &StoredSnapshot, now: Instant) -> NetworkSnapshot {
    let Some(sampled_at) = state.sampled_at else {
        return state.snapshot.clone();
    };
    let age = now.saturating_duration_since(sampled_at);
    if age <= FRESH_FOR {
        return state.snapshot.clone();
    }
    if age <= RETAIN_STALE_FOR {
        let mut snapshot = state.snapshot.clone();
        snapshot.freshness = NetworkFreshness::Stale;
        snapshot.system_traffic = NetworkCapabilityState {
            status: CapabilityAvailability::Degraded,
            reason: state
                .last_failure_reason
                .clone()
                .unwrap_or_else(|| "network_snapshot_stale".to_owned()),
        };
        degrade_supported_per_app(&mut snapshot.per_application, "network_snapshot_stale");
        return snapshot;
    }
    let mut unavailable = NetworkSnapshot::unavailable("network_snapshot_expired");
    unavailable.last_success_at_unix_ms = state.snapshot.last_success_at_unix_ms;
    if state.snapshot.per_application.status == CapabilityAvailability::Unsupported {
        unavailable.per_application = state.snapshot.per_application.clone();
    } else {
        unavailable.per_application = NetworkCapabilityState {
            status: CapabilityAvailability::Degraded,
            reason: "network_snapshot_expired".to_owned(),
        };
    }
    unavailable
}

fn degrade_supported_per_app(state: &mut NetworkCapabilityState, reason: &str) {
    if state.status != CapabilityAvailability::Unsupported {
        state.status = CapabilityAvailability::Degraded;
        state.reason = reason.to_owned();
    }
}

fn project_snapshot(
    snapshot: localdesk_network::NetworkSnapshot,
    captured_at_unix_ms: i64,
    observed_boottime_ms: u64,
    sample_interval_ms: Option<u64>,
) -> NetworkSnapshot {
    let mut system_traffic = project_capability(snapshot.system_traffic);
    let mut per_application = project_capability(snapshot.per_application);
    let applications = match project_applications(&snapshot.applications) {
        Ok(applications) => applications,
        Err(reason) => {
            per_application = NetworkCapabilityState {
                status: CapabilityAvailability::Degraded,
                reason: reason.to_owned(),
            };
            Vec::new()
        }
    };
    let freshness = match snapshot.aggregate_rate.state {
        RateState::Known => NetworkFreshness::Fresh,
        RateState::WarmingUp => {
            system_traffic = NetworkCapabilityState {
                status: CapabilityAvailability::Degraded,
                reason: snapshot.aggregate_rate.reason.to_owned(),
            };
            NetworkFreshness::WarmingUp
        }
        RateState::SamplingGap | RateState::CounterResetOrWrap | RateState::CountersUnavailable => {
            system_traffic = NetworkCapabilityState {
                status: CapabilityAvailability::Degraded,
                reason: snapshot.aggregate_rate.reason.to_owned(),
            };
            NetworkFreshness::Fresh
        }
    };
    let interfaces = snapshot
        .interfaces
        .into_iter()
        .map(|sample| NetworkInterfaceSample {
            index: sample.interface.id.index,
            name: sample.interface.id.name,
            kind: project_interface_kind(sample.interface.kind),
            kernel_kind: sample.interface.kernel_kind,
            is_up: sample.interface.is_up,
            carrier_up: sample.interface.carrier_up,
            counters: sample.interface.counters.map(|counters| NetworkByteTotals {
                rx_bytes: counters.rx_bytes,
                tx_bytes: counters.tx_bytes,
            }),
            rate: project_rate(sample.rate),
            transition: project_transition(sample.transition),
        })
        .collect();
    NetworkSnapshot {
        schema_version: localdesk_domain::NETWORK_SCHEMA_VERSION,
        snapshot_id: uuid::Uuid::new_v4(),
        captured_at_unix_ms: Some(captured_at_unix_ms),
        observed_boottime_ms: Some(observed_boottime_ms),
        sample_interval_ms,
        last_success_at_unix_ms: Some(captured_at_unix_ms),
        freshness,
        retryable: true,
        system_traffic,
        per_application,
        coverage: NetworkCoverage {
            reported_interfaces: u32::try_from(snapshot.coverage.reported_interfaces)
                .unwrap_or(u32::MAX),
            interfaces_with_counters: u32::try_from(snapshot.coverage.interfaces_with_counters)
                .unwrap_or(u32::MAX),
            includes_loopback: snapshot.coverage.includes_loopback,
            includes_tunnels: snapshot.coverage.includes_tunnels,
            layered_accounting: match snapshot.coverage.layered_accounting {
                LayeredAccounting::NotDetected => NetworkLayeredAccounting::NotDetected,
                LayeredAccounting::PossibleVpnUnderlayDoubleCounting => {
                    NetworkLayeredAccounting::PossibleVpnUnderlayDoubleCounting
                }
            },
            reason: snapshot.coverage.reason.to_owned(),
        },
        totals: Some(NetworkTrafficTotals {
            scope: NETWORK_TOTAL_SCOPE.to_owned(),
            all_interfaces: project_totals(snapshot.totals.all_interfaces),
            physical: project_totals(snapshot.totals.physical),
            loopback: project_totals(snapshot.totals.loopback),
            tunnel: project_totals(snapshot.totals.tunnel),
            other_virtual: project_totals(snapshot.totals.other_virtual),
        }),
        aggregate_rate: project_rate(snapshot.aggregate_rate),
        interfaces,
        applications,
    }
}

fn project_applications(
    applications: &[localdesk_network::ApplicationTraffic],
) -> Result<Vec<NetworkApplicationTraffic>, &'static str> {
    let rx_total = applications.iter().try_fold(0_u64, |total, application| {
        total.checked_add(application.rx_bytes)
    });
    let tx_total = applications.iter().try_fold(0_u64, |total, application| {
        total.checked_add(application.tx_bytes)
    });
    let (Some(rx_total), Some(tx_total)) = (rx_total, tx_total) else {
        return Err("per_app_total_overflow");
    };
    Ok(applications
        .iter()
        .map(|application| NetworkApplicationTraffic {
            application_key: application.application_key.clone(),
            rx_bytes: application.rx_bytes,
            tx_bytes: application.tx_bytes,
            rx_share_percent: share_percent(application.rx_bytes, rx_total),
            tx_share_percent: share_percent(application.tx_bytes, tx_total),
        })
        .collect())
}

fn share_percent(value: u64, total: u64) -> Option<f64> {
    (total > 0).then_some((value as f64) * 100.0 / (total as f64))
}

fn project_capability(state: localdesk_network::CapabilityState) -> NetworkCapabilityState {
    NetworkCapabilityState {
        status: match state.status {
            CapabilityStatus::Healthy => CapabilityAvailability::Healthy,
            CapabilityStatus::Degraded => CapabilityAvailability::Degraded,
            CapabilityStatus::Unsupported => CapabilityAvailability::Unsupported,
            CapabilityStatus::Unreachable => CapabilityAvailability::Unreachable,
        },
        reason: state.reason.to_owned(),
    }
}

fn capability_state(state: &NetworkCapabilityState) -> CapabilityRuntimeState {
    CapabilityRuntimeState::new(state.status, state.reason.clone())
}

fn project_interface_kind(kind: InterfaceKind) -> NetworkInterfaceKind {
    match kind {
        InterfaceKind::Physical => NetworkInterfaceKind::Physical,
        InterfaceKind::Loopback => NetworkInterfaceKind::Loopback,
        InterfaceKind::Tunnel => NetworkInterfaceKind::Tunnel,
        InterfaceKind::Virtual => NetworkInterfaceKind::Virtual,
    }
}

fn project_transition(transition: InterfaceTransition) -> NetworkInterfaceTransition {
    match transition {
        InterfaceTransition::Stable => NetworkInterfaceTransition::Stable,
        InterfaceTransition::FirstObservation => NetworkInterfaceTransition::FirstObservation,
        InterfaceTransition::HotplugAdded => NetworkInterfaceTransition::HotplugAdded,
        InterfaceTransition::SamplingGap => NetworkInterfaceTransition::SamplingGap,
        InterfaceTransition::CounterResetOrWrap => NetworkInterfaceTransition::CounterResetOrWrap,
        InterfaceTransition::CountersUnavailable => NetworkInterfaceTransition::CountersUnavailable,
    }
}

fn project_rate(rate: TrafficRate) -> NetworkRate {
    NetworkRate {
        rx_bytes_per_second: rate.rx_bytes_per_second,
        tx_bytes_per_second: rate.tx_bytes_per_second,
        state: match rate.state {
            RateState::Known => NetworkRateState::Known,
            RateState::WarmingUp => NetworkRateState::WarmingUp,
            RateState::SamplingGap => NetworkRateState::SamplingGap,
            RateState::CounterResetOrWrap => NetworkRateState::CounterResetOrWrap,
            RateState::CountersUnavailable => NetworkRateState::CountersUnavailable,
        },
        reason: rate.reason.to_owned(),
    }
}

fn project_totals(totals: localdesk_network::ByteTotals) -> NetworkByteTotals {
    NetworkByteTotals {
        rx_bytes: totals.rx_bytes,
        tx_bytes: totals.tx_bytes,
    }
}

fn collect_error_reason(error: &CollectError) -> &'static str {
    match error {
        CollectError::Io(_) => "rtnetlink_io_failed",
        CollectError::Protocol(_) => "rtnetlink_protocol_invalid",
        CollectError::Kernel(_) => "rtnetlink_kernel_error",
        CollectError::ReceiveTimeout { .. } => "rtnetlink_receive_timeout",
        CollectError::DumpInterrupted => "rtnetlink_dump_interrupted",
        CollectError::DumpOverrun => "rtnetlink_dump_overrun",
        CollectError::NonKernelSender { .. } => "rtnetlink_non_kernel_sender",
        CollectError::DumpLimitExceeded { .. } => "rtnetlink_dump_limit_exceeded",
    }
}

fn unix_time_ms() -> i64 {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(milliseconds).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use localdesk_network::{
        ApplicationTraffic, ByteTotals, CapabilityState, NetworkCoverage as SourceCoverage,
        NetworkSnapshot as SourceSnapshot, TrafficTotals,
    };
    use localdesk_telemetry::TelemetryManager;
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixStream;
    use tempfile::tempdir;

    fn source_snapshot(rate_state: RateState) -> SourceSnapshot {
        SourceSnapshot {
            observed_boottime: Duration::from_secs(10),
            system_traffic: CapabilityState::healthy("rtnetlink_system_counters_available"),
            per_application: CapabilityState::unsupported("unprivileged_bpf_permanently_disabled"),
            coverage: SourceCoverage {
                reported_interfaces: 0,
                interfaces_with_counters: 0,
                includes_loopback: false,
                includes_tunnels: false,
                layered_accounting: LayeredAccounting::NotDetected,
                reason: "all_reported_interfaces_have_counters",
            },
            totals: TrafficTotals {
                all_interfaces: ByteTotals::default(),
                physical: ByteTotals::default(),
                loopback: ByteTotals::default(),
                tunnel: ByteTotals::default(),
                other_virtual: ByteTotals::default(),
            },
            aggregate_rate: TrafficRate {
                rx_bytes_per_second: (rate_state == RateState::Known).then_some(1.0),
                tx_bytes_per_second: (rate_state == RateState::Known).then_some(2.0),
                state: rate_state,
                reason: match rate_state {
                    RateState::Known => "aggregate_rate_available",
                    RateState::WarmingUp => "aggregate_rate_warming_up",
                    RateState::SamplingGap => "aggregate_sampling_gap",
                    RateState::CounterResetOrWrap => "aggregate_counter_reset_or_wrap",
                    RateState::CountersUnavailable => "aggregate_counters_unavailable",
                },
            },
            interfaces: Vec::new(),
            applications: Vec::new(),
            events: Vec::new(),
        }
    }

    #[test]
    fn missing_fixed_helper_is_explicitly_unsupported() {
        let directory = tempdir().expect("helper directory");
        let telemetry = TelemetryManager::with_defaults();
        let collector = NetworkHelperCollector::with_helper_path(
            telemetry.handle(),
            Ok(directory.path().join("localdesk-network-helper")),
        );

        assert_eq!(
            collector.capability(),
            SourceCapabilityState::unsupported("network_helper_missing")
        );
    }

    #[test]
    fn helper_path_rejects_symlinks_and_group_writable_executables() {
        let directory = tempdir().expect("helper directory");
        let writable = directory.path().join("writable-helper");
        fs::write(&writable, b"fixture").expect("write fixture");
        fs::set_permissions(&writable, fs::Permissions::from_mode(0o775))
            .expect("set fixture mode");
        assert_eq!(
            validate_helper_path(&writable),
            Err("network_helper_path_rejected")
        );

        let target = directory.path().join("target-helper");
        fs::write(&target, b"fixture").expect("write target");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).expect("set target mode");
        let link = directory.path().join("linked-helper");
        symlink(&target, &link).expect("create symlink");
        assert_eq!(
            validate_helper_path(&link),
            Err("network_helper_path_rejected")
        );
    }

    #[test]
    fn cgroup_binding_paths_cannot_escape_cgroupfs() {
        assert_eq!(
            cgroup_path_on_disk("/user.slice/app.scope").expect("valid path"),
            PathBuf::from("/sys/fs/cgroup/user.slice/app.scope")
        );
        assert_eq!(
            cgroup_path_on_disk("/user.slice/../etc")
                .expect_err("parent traversal")
                .reason,
            "network_cgroup_binding_path_invalid"
        );
    }

    #[test]
    fn helper_frame_io_is_bounded_and_exact() {
        let (left, right) = UnixStream::pair().expect("socket pair");
        set_nonblocking(left.as_raw_fd()).expect("left nonblocking");
        set_nonblocking(right.as_raw_fd()).expect("right nonblocking");
        let payload = br#"{"bounded":true}"#;
        let deadline = Instant::now() + Duration::from_millis(100);

        write_helper_frame(left.as_raw_fd(), payload, deadline).expect("write helper frame");
        assert_eq!(
            read_helper_frame(right.as_raw_fd(), deadline).expect("read helper frame"),
            payload
        );
    }

    #[test]
    fn warming_rate_is_degraded_and_per_app_remains_exactly_unsupported() {
        let snapshot = project_snapshot(source_snapshot(RateState::WarmingUp), 100, 10_000, None);

        assert_eq!(snapshot.freshness, NetworkFreshness::WarmingUp);
        assert_eq!(
            snapshot.system_traffic.status,
            CapabilityAvailability::Degraded
        );
        assert_eq!(
            snapshot.per_application.status,
            CapabilityAvailability::Unsupported
        );
        assert!(snapshot.applications.is_empty());
        assert_eq!(snapshot.validate(), Ok(()));
    }

    #[test]
    fn every_unknown_aggregate_rate_degrades_the_system_capability() {
        for state in [
            RateState::SamplingGap,
            RateState::CounterResetOrWrap,
            RateState::CountersUnavailable,
        ] {
            let source = source_snapshot(state);
            let expected_reason = source.aggregate_rate.reason;
            let snapshot = project_snapshot(source, 100, 10_000, Some(1_000));
            assert_eq!(snapshot.freshness, NetworkFreshness::Fresh);
            assert_eq!(
                snapshot.system_traffic.status,
                CapabilityAvailability::Degraded
            );
            assert_eq!(snapshot.system_traffic.reason, expected_reason);
            assert_eq!(snapshot.aggregate_rate.rx_bytes_per_second, None);
            assert_eq!(snapshot.validate(), Ok(()));
        }
    }

    #[test]
    fn exact_application_counters_are_projected_to_real_shares() {
        let mut source = source_snapshot(RateState::Known);
        source.per_application = CapabilityState::healthy("core_cgroup_collector_attached");
        source.applications = vec![
            ApplicationTraffic {
                application_key: "editor.desktop".to_owned(),
                rx_bytes: 30,
                tx_bytes: 0,
            },
            ApplicationTraffic {
                application_key: "browser.desktop".to_owned(),
                rx_bytes: 70,
                tx_bytes: 10,
            },
        ];

        let snapshot = project_snapshot(source, 100, 10_000, Some(1_000));

        assert_eq!(
            snapshot.per_application.status,
            CapabilityAvailability::Healthy
        );
        assert_eq!(snapshot.applications.len(), 2);
        assert_eq!(snapshot.applications[0].rx_share_percent, Some(30.0));
        assert_eq!(snapshot.applications[0].tx_share_percent, Some(0.0));
        assert_eq!(snapshot.applications[1].rx_share_percent, Some(70.0));
        assert_eq!(snapshot.applications[1].tx_share_percent, Some(100.0));
        assert_eq!(snapshot.validate(), Ok(()));
    }

    #[test]
    fn zero_direction_total_keeps_share_unknown() {
        let applications = project_applications(&[ApplicationTraffic {
            application_key: "idle.desktop".to_owned(),
            rx_bytes: 0,
            tx_bytes: 0,
        }])
        .expect("bounded counters");

        assert_eq!(applications[0].rx_share_percent, None);
        assert_eq!(applications[0].tx_share_percent, None);
    }

    #[test]
    fn retained_snapshot_becomes_stale_then_unavailable() {
        let sampled_at = Instant::now();
        let state = StoredSnapshot {
            snapshot: project_snapshot(source_snapshot(RateState::Known), 100, 10_000, Some(1_000)),
            sampled_at: Some(sampled_at),
            last_failure_reason: None,
        };

        let stale = snapshot_with_current_freshness(&state, sampled_at + Duration::from_secs(4));
        assert_eq!(stale.freshness, NetworkFreshness::Stale);
        assert_eq!(
            stale.system_traffic.status,
            CapabilityAvailability::Degraded
        );

        let expired = snapshot_with_current_freshness(&state, sampled_at + Duration::from_secs(11));
        assert_eq!(
            expired.system_traffic.status,
            CapabilityAvailability::Unreachable
        );
        assert_eq!(expired.totals, None);
        assert_eq!(
            expired.per_application.status,
            CapabilityAvailability::Unsupported
        );
    }

    #[test]
    fn supported_per_app_facts_degrade_when_the_snapshot_is_stale_or_expired() {
        let sampled_at = Instant::now();
        let mut snapshot =
            project_snapshot(source_snapshot(RateState::Known), 100, 10_000, Some(1_000));
        snapshot.per_application = NetworkCapabilityState {
            status: CapabilityAvailability::Healthy,
            reason: "core_cgroup_collector_attached".to_owned(),
        };
        snapshot.applications.push(NetworkApplicationTraffic {
            application_key: "editor.desktop".to_owned(),
            rx_bytes: 10,
            tx_bytes: 20,
            rx_share_percent: Some(100.0),
            tx_share_percent: Some(100.0),
        });
        let state = StoredSnapshot {
            snapshot,
            sampled_at: Some(sampled_at),
            last_failure_reason: None,
        };

        let stale = snapshot_with_current_freshness(&state, sampled_at + Duration::from_secs(4));
        assert_eq!(
            stale.per_application.status,
            CapabilityAvailability::Degraded
        );
        assert_eq!(stale.per_application.reason, "network_snapshot_stale");
        assert_eq!(stale.applications.len(), 1);

        let expired = snapshot_with_current_freshness(&state, sampled_at + Duration::from_secs(11));
        assert_eq!(
            expired.per_application.status,
            CapabilityAvailability::Degraded
        );
        assert_eq!(expired.per_application.reason, "network_snapshot_expired");
        assert!(expired.applications.is_empty());
    }
}
