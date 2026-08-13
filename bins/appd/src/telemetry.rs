use localdesk_telemetry::{
    SampleGeneration, TelemetryError, TelemetryManager, TelemetryManagerHandle,
    TelemetryStoreConfig,
};
use localdesk_telemetry_helper_protocol::{
    CollectionReply, CollectionRequest, FrameError, MAX_FRAME_BYTES, decode_reply, encode,
};
use std::{io, path::PathBuf, process::Stdio, sync::Arc, time::Duration};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{Mutex, oneshot, watch},
    time::{Instant, interval, timeout},
};

pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
pub const SAMPLE_DEADLINE: Duration = Duration::from_secs(2);
pub const FRESH_AFTER: Duration = Duration::from_millis(2_500);
pub const MAX_STALE: Duration = Duration::from_secs(10);
#[allow(dead_code)]
pub const HELPER_NAME: &str = "localdesk-telemetry-helper";

const INITIAL_RESTART_BACKOFF: Duration = Duration::from_millis(100);
const MAX_RESTART_BACKOFF: Duration = Duration::from_secs(5);

pub fn store_config() -> TelemetryStoreConfig {
    TelemetryStoreConfig {
        stale_after: FRESH_AFTER,
        max_stale: MAX_STALE,
    }
}

#[derive(Debug, Error)]
pub enum HelperFailure {
    #[error("telemetry helper executable is unavailable")]
    HelperUnavailable,
    #[error("telemetry helper executable is not executable")]
    HelperUnexecutable,
    #[error("telemetry helper could not be spawned")]
    Spawn(#[source] io::Error),
    #[error("telemetry helper stdio is unavailable")]
    MissingPipe,
    #[error("telemetry helper framing failed: {0}")]
    Frame(#[source] FrameError),
    #[error("telemetry helper I/O failed: {0}")]
    Io(#[source] io::Error),
    #[error("telemetry helper generation did not match the request")]
    GenerationMismatch,
    #[error("telemetry helper sample exceeded its deadline")]
    Timeout,
    #[error("telemetry helper wait failed: {0}")]
    Wait(#[source] io::Error),
}

impl HelperFailure {
    pub const fn reason_code(&self) -> &'static str {
        match self {
            Self::HelperUnavailable => "helper_missing",
            Self::HelperUnexecutable => "helper_unexecutable",
            Self::Spawn(_) => "helper_spawn_failed",
            Self::MissingPipe => "helper_stdio_unavailable",
            Self::Frame(_) => "helper_protocol_error",
            Self::Io(_) => "helper_eof",
            Self::GenerationMismatch => "helper_generation_mismatch",
            Self::Timeout => "helper_timeout",
            Self::Wait(_) => "helper_wait_failed",
        }
    }

    pub const fn retryable(&self) -> bool {
        !matches!(
            self,
            Self::HelperUnavailable | Self::HelperUnexecutable | Self::MissingPipe
        )
    }

    pub fn as_telemetry_error(&self) -> TelemetryError {
        TelemetryError::collection(self.reason_code(), self.retryable(), self.reason_code())
    }
}

#[derive(Clone, Debug)]
pub struct ChildReaper {
    child: Arc<Mutex<Option<Child>>>,
}

impl ChildReaper {
    pub fn new() -> Self {
        Self {
            child: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) async fn install(&self, child: Child) -> Result<(), HelperFailure> {
        let mut slot = self.child.lock().await;
        if slot.is_some() {
            return Err(HelperFailure::Spawn(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "telemetry helper generation is still active",
            )));
        }
        *slot = Some(child);
        Ok(())
    }

    pub async fn start_kill(&self) -> Result<bool, io::Error> {
        let mut slot = self.child.lock().await;
        let Some(child) = slot.as_mut() else {
            return Ok(false);
        };
        child.start_kill()?;
        Ok(true)
    }

    pub async fn wait(&self) -> Result<bool, io::Error> {
        let mut slot = self.child.lock().await;
        let Some(child) = slot.as_mut() else {
            return Ok(false);
        };
        child.wait().await?;
        *slot = None;
        Ok(true)
    }

    pub async fn kill_and_wait(&self) -> Result<(), io::Error> {
        let kill_result = self.start_kill().await;
        let wait_result = self.wait().await;
        match (kill_result, wait_result) {
            (Ok(_), Ok(_)) => Ok(()),
            (Err(error), _) => Err(error),
            (_, Err(error)) => Err(error),
        }
    }

    #[allow(dead_code)]
    pub async fn ensure_reaped_until(&self, deadline: Instant) {
        let _ = self.start_kill().await;
        if Instant::now() < deadline {
            let _ = tokio::time::timeout_at(deadline, self.wait()).await;
        }
        self.try_reap().await;
    }

    #[allow(dead_code)]
    async fn try_reap(&self) {
        let mut slot = self.child.lock().await;
        let Some(child) = slot.as_mut() else {
            return;
        };
        if matches!(child.try_wait(), Ok(Some(_))) {
            *slot = None;
        }
    }
}

impl Default for ChildReaper {
    fn default() -> Self {
        Self::new()
    }
}

struct HelperSession {
    stdin: ChildStdin,
    stdout: ChildStdout,
}

impl HelperSession {
    async fn collect(
        &mut self,
        generation: SampleGeneration,
    ) -> Result<CollectionReply, HelperFailure> {
        let request = CollectionRequest::collect(generation);
        let payload = encode(&request).map_err(HelperFailure::Frame)?;
        write_async_frame(&mut self.stdin, &payload)
            .await
            .map_err(map_frame_io_error)?;
        let payload = read_async_frame(&mut self.stdout)
            .await
            .map_err(map_frame_io_error)?;
        let reply = decode_reply(&payload).map_err(HelperFailure::Frame)?;
        if reply.generation != generation {
            return Err(HelperFailure::GenerationMismatch);
        }
        Ok(reply)
    }
}

pub struct TelemetrySupervisor {
    manager: TelemetryManager,
    helper_path: Result<PathBuf, HelperFailure>,
    reaper: ChildReaper,
    session: Option<HelperSession>,
    retry_after: Option<Instant>,
    restart_backoff: Duration,
}

impl TelemetrySupervisor {
    #[allow(dead_code)]
    pub fn new(manager: TelemetryManager) -> Self {
        Self {
            manager,
            helper_path: production_helper_path(),
            reaper: ChildReaper::new(),
            session: None,
            retry_after: None,
            restart_backoff: INITIAL_RESTART_BACKOFF,
        }
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub fn with_helper_path(manager: TelemetryManager, path: PathBuf) -> Self {
        Self {
            manager,
            helper_path: Ok(path),
            reaper: ChildReaper::new(),
            session: None,
            retry_after: None,
            restart_backoff: INITIAL_RESTART_BACKOFF,
        }
    }

    pub fn handle(&self) -> TelemetryManagerHandle {
        self.manager.handle()
    }

    pub fn cleanup_handle(&self) -> ChildReaper {
        self.reaper.clone()
    }

    pub async fn run(mut self, mut shutdown: watch::Receiver<bool>, kill_ack: oneshot::Sender<()>) {
        let mut ticker = interval(SAMPLE_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        self.shutdown(kill_ack).await;
                        return;
                    }
                }
                _ = ticker.tick() => {
                    if *shutdown.borrow() {
                        self.shutdown(kill_ack).await;
                        return;
                    }
                    let now = Instant::now();
                    if self.retry_after.is_some_and(|retry_after| now < retry_after) {
                        continue;
                    }

                    let generation = match self.manager.begin_sample() {
                        Ok(generation) => generation,
                        Err(error) => {
                            tracing::error!(%error, "telemetry manager could not start a sample");
                            continue;
                        }
                    };

                    if self.session.is_none()
                        && let Err(error) = self.start_session().await
                    {
                        self.publish_error(generation, error.as_telemetry_error());
                        self.schedule_restart();
                        continue;
                    }

                    let outcome = match self.session.as_mut() {
                        Some(session) => {
                            tokio::select! {
                                result = timeout(SAMPLE_DEADLINE, session.collect(generation)) => {
                                    Some(match result {
                                        Ok(result) => result,
                                        Err(_) => Err(HelperFailure::Timeout),
                                    })
                                }
                                changed = shutdown.changed() => {
                                    if changed.is_err() || *shutdown.borrow() {
                                        None
                                    } else {
                                        Some(Err(HelperFailure::Io(io::Error::new(
                                            io::ErrorKind::Interrupted,
                                            "telemetry shutdown channel changed",
                                        ))))
                                    }
                                }
                            }
                        }
                        None => Some(Err(HelperFailure::HelperUnavailable)),
                    };

                    let Some(outcome) = outcome else {
                        self.shutdown(kill_ack).await;
                        return;
                    };

                    if *shutdown.borrow() {
                        self.shutdown(kill_ack).await;
                        return;
                    }

                    match outcome {
                        Ok(reply) => match self.manager.accept_reply(reply) {
                            Ok(_) => self.reset_restart_backoff(),
                            Err(error) => {
                                let should_restart = matches!(error, TelemetryError::InvalidReply(_));
                                self.publish_error(generation, error);
                                if should_restart {
                                    let _ = self.drop_session_and_wait().await;
                                    self.schedule_restart();
                                }
                            }
                        },
                        Err(error) => {
                            let telemetry_error = error.as_telemetry_error();
                            if let Some(wait_error) = self.drop_session_and_wait().await {
                                tracing::warn!(%wait_error, "telemetry helper wait failed after collection error");
                            }
                            self.publish_error(generation, telemetry_error);
                            self.schedule_restart();
                        }
                    }
                }
            }
        }
    }

    async fn start_session(&mut self) -> Result<(), HelperFailure> {
        self.reaper
            .kill_and_wait()
            .await
            .map_err(HelperFailure::Wait)?;
        let path = match &self.helper_path {
            Ok(path) => path.clone(),
            Err(_) => return Err(HelperFailure::HelperUnavailable),
        };
        let mut command = Command::new(path);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let child = command.spawn().map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                HelperFailure::HelperUnavailable
            } else if error.kind() == io::ErrorKind::PermissionDenied {
                HelperFailure::HelperUnexecutable
            } else {
                HelperFailure::Spawn(error)
            }
        })?;
        let mut child = child;
        let Some(stdin) = child.stdin.take() else {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(HelperFailure::MissingPipe);
        };
        let Some(stdout) = child.stdout.take() else {
            drop(stdin);
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(HelperFailure::MissingPipe);
        };
        self.reaper.install(child).await?;
        self.session = Some(HelperSession { stdin, stdout });
        Ok(())
    }

    async fn drop_session_and_wait(&mut self) -> Option<HelperFailure> {
        self.session.take();
        self.reaper
            .kill_and_wait()
            .await
            .err()
            .map(HelperFailure::Wait)
    }

    async fn shutdown(&mut self, kill_ack: oneshot::Sender<()>) {
        self.latch_shutdown().await;
        let _ = self.reaper.start_kill().await;
        let _ = kill_ack.send(());
        self.drop_session_and_wait().await;
    }

    async fn latch_shutdown(&mut self) {
        let _ = self.manager.begin_sample();
        if let Err(error) = self.manager.shutdown() {
            tracing::warn!(%error, "telemetry manager shutdown state could not be published");
        }
    }

    fn publish_error(&mut self, generation: SampleGeneration, error: TelemetryError) {
        if let Err(publish_error) = self.manager.accept_error(generation, error) {
            tracing::warn!(%publish_error, "telemetry collection failure could not be published");
        }
    }

    fn schedule_restart(&mut self) {
        self.retry_after = Some(Instant::now() + self.restart_backoff);
        self.restart_backoff = self
            .restart_backoff
            .checked_mul(2)
            .unwrap_or(MAX_RESTART_BACKOFF)
            .min(MAX_RESTART_BACKOFF);
    }

    fn reset_restart_backoff(&mut self) {
        self.retry_after = None;
        self.restart_backoff = INITIAL_RESTART_BACKOFF;
    }
}

#[allow(dead_code)]
fn production_helper_path() -> Result<PathBuf, HelperFailure> {
    let executable = std::env::current_exe().map_err(|_| HelperFailure::HelperUnavailable)?;
    let parent = executable
        .parent()
        .ok_or(HelperFailure::HelperUnavailable)?;
    Ok(parent.join(HELPER_NAME))
}

fn map_frame_io_error(error: FrameIoError) -> HelperFailure {
    match error {
        FrameIoError::Frame(error) => HelperFailure::Frame(error),
        FrameIoError::Io(error) => HelperFailure::Io(error),
    }
}

#[derive(Debug, Error)]
enum FrameIoError {
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

async fn write_async_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    payload: &[u8],
) -> Result<(), FrameIoError> {
    if payload.is_empty() {
        return Err(FrameError::Empty.into());
    }
    if payload.len() > MAX_FRAME_BYTES {
        return Err(FrameError::Oversized {
            length: payload.len(),
            max: MAX_FRAME_BYTES,
        }
        .into());
    }
    let length = u32::try_from(payload.len()).map_err(|_| FrameError::LengthOverflow)?;
    writer.write_all(&length.to_be_bytes()).await?;
    writer.write_all(payload).await?;
    writer.flush().await?;
    Ok(())
}

async fn read_async_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Vec<u8>, FrameIoError> {
    let mut prefix = [0_u8; 4];
    reader.read_exact(&mut prefix).await?;
    let length = u32::from_be_bytes(prefix) as usize;
    if length == 0 {
        return Err(FrameError::Empty.into());
    }
    if length > MAX_FRAME_BYTES {
        return Err(FrameError::Oversized {
            length,
            max: MAX_FRAME_BYTES,
        }
        .into());
    }
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload).await?;
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::process::Command;

    #[test]
    fn appd_uses_the_frozen_freshness_window() {
        let config = store_config();
        assert_eq!(config.stale_after, Duration::from_millis(2_500));
        assert_eq!(config.max_stale, Duration::from_secs(10));
    }

    #[tokio::test]
    async fn reaper_kills_and_reaps_a_test_child() {
        let child = Command::new("/bin/sleep")
            .arg("60")
            .spawn()
            .expect("test child");
        let reaper = ChildReaper::new();
        reaper.install(child).await.expect("install child");
        assert!(reaper.start_kill().await.expect("start kill"));
        assert!(reaper.wait().await.expect("wait"));
        assert!(!reaper.wait().await.expect("second wait"));
    }
}
