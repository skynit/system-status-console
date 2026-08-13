use crate::{
    Authentication, Endpoint, HostKeyPolicy, ProfileError, PtyError, PtySize, SshProfile,
    askpass::AskpassSecret, pty::PtySession,
};
use openssh_sftp_client::{Sftp, SftpOptions};
use std::{
    ffi::OsString,
    fmt,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::Arc,
    thread,
    time::Duration,
};
use tempfile::{Builder, NamedTempFile};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::{Child as TokioChild, Command as TokioCommand},
    sync::Mutex as AsyncMutex,
    task::JoinHandle,
};

const SSH_PROGRAM: &str = "/usr/bin/ssh";
const SFTP_PROGRAM: &str = "/usr/bin/sftp";
const TARGET_ALIAS: &str = "localdesk-target";
const CONNECT_TIMEOUT_SECONDS: u8 = 15;
pub const MAX_TERMINAL_OUTPUT_BYTES: usize = 64 * 1024;
pub const MAX_TERMINAL_INPUT_BYTES: usize = 64 * 1024;
pub const MAX_TERMINAL_TRANSCRIPT_BYTES: usize = 64 * 1024;
pub const MAX_SFTP_STDOUT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_SFTP_STDERR_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct TerminalCapabilities {
    pub max_output_chunk_bytes: usize,
    pub max_pending_output_bytes: usize,
    pub max_input_chunk_bytes: usize,
    pub max_transcript_bytes: usize,
    pub max_rows: u16,
    pub max_columns: u16,
    pub max_pixel_dimension: u16,
    pub nonblocking_output: bool,
    pub fixed_openssh_program: bool,
}

pub const TERMINAL_CAPABILITIES: TerminalCapabilities = TerminalCapabilities {
    max_output_chunk_bytes: MAX_TERMINAL_OUTPUT_BYTES,
    max_pending_output_bytes: MAX_TERMINAL_OUTPUT_BYTES,
    max_input_chunk_bytes: MAX_TERMINAL_INPUT_BYTES,
    max_transcript_bytes: MAX_TERMINAL_TRANSCRIPT_BYTES,
    max_rows: crate::pty::MAX_PTY_ROWS,
    max_columns: crate::pty::MAX_PTY_COLUMNS,
    max_pixel_dimension: crate::pty::MAX_PTY_PIXEL_DIMENSION,
    nonblocking_output: true,
    fixed_openssh_program: true,
};

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum SftpOperation {
    List {
        remote_path: String,
    },
    Stat {
        remote_path: String,
    },
    Download {
        remote_path: String,
        local_path: PathBuf,
    },
    Upload {
        local_path: PathBuf,
        remote_path: String,
    },
    CreateDirectory {
        remote_path: String,
    },
    RemoveFile {
        remote_path: String,
    },
    RemoveDirectory {
        remote_path: String,
    },
    Rename {
        from: String,
        to: String,
    },
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum DisconnectReason {
    HostKeyChanged,
    HostKeyRevoked,
    HostKeyUnknown,
    AuthenticationFailed,
    NetworkUnreachable,
    ConnectionLost,
    OpenSshFailure,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum SessionState {
    Running,
    Exited { code: Option<i32> },
    Disconnected { reason: DisconnectReason },
    ClosedByClient,
}

#[derive(Clone, Eq, PartialEq)]
pub struct TerminalOutput {
    bytes: Vec<u8>,
}

impl TerminalOutput {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

impl fmt::Debug for TerminalOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalOutput")
            .field("byte_count", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum TerminalRead {
    Pending,
    Data(TerminalOutput),
    EndOfStream,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TerminalStatus {
    pub state: SessionState,
    pub pending_output_bytes: usize,
    pub pending_output_dropped_bytes: u64,
    pub transcript_retained_bytes: usize,
    pub transcript_dropped_bytes: u64,
}

#[derive(Debug, Error)]
pub enum TerminalError {
    #[error("terminal read limit {requested} is outside 1..={maximum} bytes")]
    InvalidReadLimit { requested: usize, maximum: usize },
    #[error("terminal input is {provided} bytes; maximum is {maximum} bytes")]
    InputTooLarge { provided: usize, maximum: usize },
    #[error("failed to read terminal output: {0}")]
    Read(#[source] io::Error),
    #[error("failed to write terminal input: {0}")]
    Write(#[source] io::Error),
    #[error(transparent)]
    Pty(#[from] PtyError),
}

impl TerminalError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidReadLimit { .. } => "terminal_read_limit_invalid",
            Self::InputTooLarge { .. } => "terminal_input_too_large",
            Self::Read(_) => "terminal_read_failed",
            Self::Write(_) => "terminal_write_failed",
            Self::Pty(PtyError::InvalidSize { .. }) => "terminal_size_invalid",
            Self::Pty(_) => "terminal_pty_failed",
        }
    }

    pub fn retryable(&self) -> bool {
        matches!(self, Self::Read(_) | Self::Write(_))
    }
}

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error(transparent)]
    InvalidProfile(#[from] ProfileError),
    #[error("SFTP operation list must not be empty")]
    EmptySftpBatch,
    #[error("{field} is empty or contains NUL/newline")]
    InvalidSftpPath { field: String },
    #[error("{field} must be an absolute UTF-8 local path")]
    InvalidLocalPath { field: String },
    #[error("failed to create private OpenSSH input: {0}")]
    CreateInput(#[source] io::Error),
    #[error("failed to write private OpenSSH input: {0}")]
    WriteInput(#[source] io::Error),
    #[error(transparent)]
    Pty(#[from] PtyError),
    #[error("failed to spawn fixed OpenSSH SFTP process: {0}")]
    SpawnSftp(#[source] io::Error),
    #[error("failed to inspect OpenSSH SFTP process: {0}")]
    InspectSftp(#[source] io::Error),
    #[error("failed to close OpenSSH SFTP process: {0}")]
    CloseSftp(#[source] io::Error),
    #[error("structured OpenSSH SFTP handshake failed: {reason:?}")]
    StructuredSftpHandshake {
        reason: DisconnectReason,
        #[source]
        source: openssh_sftp_client::Error,
    },
    #[error("failed to initialize structured OpenSSH SFTP transport: {0}")]
    StructuredSftp(#[source] openssh_sftp_client::Error),
    #[error("OpenSSH SFTP {stream} exceeded the {maximum}-byte capture limit")]
    SftpOutputLimit {
        stream: &'static str,
        maximum: usize,
    },
}

#[derive(Debug, Default, Clone, Copy)]
pub struct OpenSshAdapter;

impl OpenSshAdapter {
    pub(crate) fn open_terminal(
        &self,
        profile: &SshProfile,
        size: PtySize,
        askpass: Option<&AskpassSecret>,
    ) -> Result<TerminalSession, AdapterError> {
        let prepared = PreparedConnection::new(profile)?;
        let mut command = fixed_command(SSH_PROGRAM, prepared.terminal_args());
        if let Some(askpass) = askpass {
            askpass.configure_std_command(&mut command);
        }
        let pty = PtySession::spawn(&mut command, size)?;
        Ok(TerminalSession {
            pty,
            pending_output: BoundedPendingOutput::default(),
            transcript: BoundedTranscript::default(),
            _config: prepared.config,
        })
    }

    pub fn start_sftp(
        &self,
        profile: &SshProfile,
        operations: &[SftpOperation],
    ) -> Result<SftpSession, AdapterError> {
        self.start_sftp_with_askpass(profile, operations, None)
    }

    pub(crate) fn start_sftp_with_askpass(
        &self,
        profile: &SshProfile,
        operations: &[SftpOperation],
        askpass: Option<&AskpassSecret>,
    ) -> Result<SftpSession, AdapterError> {
        let prepared = PreparedConnection::new(profile)?;
        let batch = write_private_file("localdesk-sftp-batch", &render_sftp_batch(operations)?)?;
        let args = prepared.sftp_args(batch.path());
        let mut command = fixed_command(SFTP_PROGRAM, args);
        if let Some(askpass) = askpass {
            askpass.configure_std_command(&mut command);
        }
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(AdapterError::SpawnSftp)?;
        Ok(SftpSession {
            child: Some(child),
            _config: prepared.config,
            _batch: batch,
        })
    }

    pub(crate) async fn start_structured_sftp(
        &self,
        profile: &SshProfile,
        askpass: Option<&AskpassSecret>,
    ) -> Result<StructuredSftpSession, AdapterError> {
        let prepared = PreparedConnection::new(profile)?;
        let mut command = tokio_fixed_command(SSH_PROGRAM, prepared.structured_sftp_args());
        if let Some(askpass) = askpass {
            askpass.configure_tokio_command(&mut command);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(AdapterError::SpawnSftp)?;
        let stdin = child.stdin.take().ok_or_else(|| {
            AdapterError::InspectSftp(io::Error::other(
                "structured SFTP stdin pipe is unavailable",
            ))
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            AdapterError::InspectSftp(io::Error::other(
                "structured SFTP stdout pipe is unavailable",
            ))
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            AdapterError::InspectSftp(io::Error::other(
                "structured SFTP stderr pipe is unavailable",
            ))
        })?;
        let process = StructuredSftpProcess {
            child: Arc::new(AsyncMutex::new(Some(child))),
        };
        let stderr_task = tokio::spawn(read_bounded_async(stderr, MAX_SFTP_STDERR_BYTES));
        let sftp = match Sftp::new(
            stdin,
            stdout,
            SftpOptions::new().flush_interval(Duration::ZERO),
        )
        .await
        {
            Ok(sftp) => sftp,
            Err(error) => {
                let _ = process.terminate().await;
                let stderr = match stderr_task.await {
                    Ok(Ok(output)) if output.dropped_bytes == 0 => output.bytes,
                    Ok(Ok(_)) => {
                        return Err(AdapterError::SftpOutputLimit {
                            stream: "stderr",
                            maximum: MAX_SFTP_STDERR_BYTES,
                        });
                    }
                    Ok(Err(error)) => return Err(AdapterError::InspectSftp(error)),
                    Err(_) => {
                        return Err(AdapterError::InspectSftp(io::Error::other(
                            "structured SFTP stderr reader panicked",
                        )));
                    }
                };
                return Err(AdapterError::StructuredSftpHandshake {
                    reason: classify_disconnect_reason(&stderr),
                    source: error,
                });
            }
        };
        Ok(StructuredSftpSession {
            sftp: Some(sftp),
            process,
            stderr_task: Some(stderr_task),
            _config: prepared.config,
        })
    }
}

#[derive(Clone)]
pub(crate) struct StructuredSftpProcess {
    child: Arc<AsyncMutex<Option<TokioChild>>>,
}

impl fmt::Debug for StructuredSftpProcess {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StructuredSftpProcess")
            .finish_non_exhaustive()
    }
}

impl StructuredSftpProcess {
    pub(crate) async fn terminate(&self) -> Result<(), AdapterError> {
        let mut process = self.child.lock().await;
        let Some(child) = process.as_mut() else {
            return Ok(());
        };
        if child
            .try_wait()
            .map_err(AdapterError::InspectSftp)?
            .is_none()
        {
            child.start_kill().map_err(AdapterError::CloseSftp)?;
        }
        child.wait().await.map_err(AdapterError::CloseSftp)?;
        *process = None;
        Ok(())
    }
}

pub(crate) struct StructuredSftpSession {
    sftp: Option<Sftp>,
    process: StructuredSftpProcess,
    stderr_task: Option<JoinHandle<io::Result<BoundedRead>>>,
    _config: NamedTempFile,
}

impl fmt::Debug for StructuredSftpSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StructuredSftpSession")
            .field("process", &self.process)
            .finish_non_exhaustive()
    }
}

impl StructuredSftpSession {
    pub(crate) fn client(&self) -> &Sftp {
        self.sftp.as_ref().expect("live structured SFTP client")
    }

    pub(crate) fn process(&self) -> StructuredSftpProcess {
        self.process.clone()
    }

    pub(crate) async fn close(mut self) -> Result<(), AdapterError> {
        let close_result = if let Some(sftp) = self.sftp.take() {
            match tokio::time::timeout(Duration::from_secs(1), sftp.close()).await {
                Ok(result) => result.map_err(AdapterError::StructuredSftp),
                Err(_) => {
                    let _ = self.process.terminate().await;
                    Err(AdapterError::CloseSftp(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "structured SFTP close timed out",
                    )))
                }
            }
        } else {
            Ok(())
        };
        let process_result = self.process.terminate().await;
        if let Some(task) = self.stderr_task.take() {
            match task.await {
                Ok(Ok(output)) if output.dropped_bytes != 0 => {
                    return Err(AdapterError::SftpOutputLimit {
                        stream: "stderr",
                        maximum: MAX_SFTP_STDERR_BYTES,
                    });
                }
                Ok(Ok(_)) => {}
                Ok(Err(error)) => return Err(AdapterError::InspectSftp(error)),
                Err(_) => {
                    return Err(AdapterError::InspectSftp(io::Error::other(
                        "structured SFTP stderr reader panicked",
                    )));
                }
            }
        }
        close_result.and(process_result)
    }
}

pub struct TerminalSession {
    pty: PtySession,
    pending_output: BoundedPendingOutput,
    transcript: BoundedTranscript,
    _config: NamedTempFile,
}

impl TerminalSession {
    pub fn process_id(&self) -> u32 {
        self.pty.process_id()
    }

    pub fn capabilities(&self) -> TerminalCapabilities {
        TERMINAL_CAPABILITIES
    }

    pub fn resize(&self, size: PtySize) -> Result<(), TerminalError> {
        self.pty.resize(size).map_err(TerminalError::Pty)
    }

    pub fn read_output(&mut self, max_bytes: usize) -> Result<TerminalRead, TerminalError> {
        if max_bytes == 0 || max_bytes > MAX_TERMINAL_OUTPUT_BYTES {
            return Err(TerminalError::InvalidReadLimit {
                requested: max_bytes,
                maximum: MAX_TERMINAL_OUTPUT_BYTES,
            });
        }

        if let Some(bytes) = self.pending_output.take(max_bytes) {
            return Ok(TerminalRead::Data(TerminalOutput { bytes }));
        }

        let mut bytes = vec![0_u8; max_bytes];
        match self.pty.read(&mut bytes) {
            Ok(0) => Ok(TerminalRead::EndOfStream),
            Ok(count) => {
                bytes.truncate(count);
                self.transcript.push(&bytes);
                Ok(TerminalRead::Data(TerminalOutput { bytes }))
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(TerminalRead::Pending),
            Err(error) if error.raw_os_error() == Some(nix::libc::EIO) => {
                if self.pty.try_wait()?.is_some() {
                    Ok(TerminalRead::EndOfStream)
                } else {
                    Err(TerminalError::Read(error))
                }
            }
            Err(error) => Err(TerminalError::Read(error)),
        }
    }

    pub fn write_input(&mut self, input: &[u8]) -> Result<(), TerminalError> {
        if input.len() > MAX_TERMINAL_INPUT_BYTES {
            return Err(TerminalError::InputTooLarge {
                provided: input.len(),
                maximum: MAX_TERMINAL_INPUT_BYTES,
            });
        }
        self.pty.write_all(input).map_err(TerminalError::Write)
    }

    pub fn poll_state(&mut self) -> Result<TerminalStatus, TerminalError> {
        let capture = self.capture_pending_output()?;
        let exit = self.pty.try_wait()?;
        if capture == OutputCapture::BudgetExhausted {
            return Ok(self.status(SessionState::Running));
        }
        Ok(self.status(classify_session(
            exit.as_ref(),
            self.transcript.as_bytes(),
            self.pty.close_requested(),
        )))
    }

    pub fn close(&mut self) -> Result<TerminalStatus, TerminalError> {
        self.pty.close()?;
        Ok(self.status(SessionState::ClosedByClient))
    }

    fn status(&self, state: SessionState) -> TerminalStatus {
        TerminalStatus {
            state,
            pending_output_bytes: self.pending_output.len(),
            pending_output_dropped_bytes: self.pending_output.dropped_bytes(),
            transcript_retained_bytes: self.transcript.as_bytes().len(),
            transcript_dropped_bytes: self.transcript.dropped_bytes(),
        }
    }

    fn capture_pending_output(&mut self) -> Result<OutputCapture, TerminalError> {
        let mut captured = 0_usize;
        let mut buffer = [0_u8; 8 * 1024];
        while captured < MAX_TERMINAL_OUTPUT_BYTES {
            let remaining = MAX_TERMINAL_OUTPUT_BYTES - captured;
            let read_limit = remaining.min(buffer.len());
            match self.pty.read(&mut buffer[..read_limit]) {
                Ok(0) => return Ok(OutputCapture::Drained),
                Ok(count) => {
                    let bytes = &buffer[..count];
                    self.pending_output.push(bytes);
                    self.transcript.push(bytes);
                    captured += count;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    return Ok(OutputCapture::Drained);
                }
                Err(error) if error.raw_os_error() == Some(nix::libc::EIO) => {
                    return Ok(OutputCapture::Drained);
                }
                Err(error) => return Err(TerminalError::Read(error)),
            }
        }
        Ok(OutputCapture::BudgetExhausted)
    }
}

impl fmt::Debug for TerminalSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalSession")
            .field("process_id", &self.process_id())
            .field("pending_output_bytes", &self.pending_output.len())
            .field(
                "pending_output_dropped_bytes",
                &self.pending_output.dropped_bytes(),
            )
            .field(
                "transcript_retained_bytes",
                &self.transcript.as_bytes().len(),
            )
            .field("transcript_dropped_bytes", &self.transcript.dropped_bytes())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum OutputCapture {
    Drained,
    BudgetExhausted,
}

#[derive(Default)]
struct BoundedPendingOutput {
    bytes: Vec<u8>,
    dropped_bytes: u64,
}

impl BoundedPendingOutput {
    fn push(&mut self, incoming: &[u8]) {
        let total = self.bytes.len().saturating_add(incoming.len());
        if total > MAX_TERMINAL_OUTPUT_BYTES {
            let overflow = total - MAX_TERMINAL_OUTPUT_BYTES;
            let remove_existing = overflow.min(self.bytes.len());
            self.bytes.drain(..remove_existing);
            let skip_incoming = overflow - remove_existing;
            self.dropped_bytes = self.dropped_bytes.saturating_add(overflow as u64);
            self.bytes.extend_from_slice(&incoming[skip_incoming..]);
        } else {
            self.bytes.extend_from_slice(incoming);
        }
    }

    fn take(&mut self, maximum: usize) -> Option<Vec<u8>> {
        if self.bytes.is_empty() {
            return None;
        }
        let count = maximum.min(self.bytes.len());
        Some(self.bytes.drain(..count).collect())
    }

    fn len(&self) -> usize {
        self.bytes.len()
    }

    fn dropped_bytes(&self) -> u64 {
        self.dropped_bytes
    }
}

#[derive(Default)]
struct BoundedTranscript {
    bytes: Vec<u8>,
    dropped_bytes: u64,
}

impl BoundedTranscript {
    fn push(&mut self, incoming: &[u8]) {
        let total = self.bytes.len().saturating_add(incoming.len());
        if total > MAX_TERMINAL_TRANSCRIPT_BYTES {
            let overflow = total - MAX_TERMINAL_TRANSCRIPT_BYTES;
            let remove_existing = overflow.min(self.bytes.len());
            self.bytes.drain(..remove_existing);
            let skip_incoming = overflow - remove_existing;
            self.dropped_bytes = self.dropped_bytes.saturating_add(overflow as u64);
            self.bytes.extend_from_slice(&incoming[skip_incoming..]);
        } else {
            self.bytes.extend_from_slice(incoming);
        }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn dropped_bytes(&self) -> u64 {
        self.dropped_bytes
    }
}

pub struct SftpSession {
    child: Option<Child>,
    _config: NamedTempFile,
    _batch: NamedTempFile,
}

pub struct SftpOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl fmt::Debug for SftpOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SftpOutput")
            .field("status", &self.status)
            .field("stdout_bytes", &self.stdout.len())
            .field("stderr_bytes", &self.stderr.len())
            .finish_non_exhaustive()
    }
}

impl SftpSession {
    pub fn process_id(&self) -> u32 {
        self.child.as_ref().expect("live SFTP child").id()
    }

    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>, AdapterError> {
        let Some(child) = self.child.as_mut() else {
            return Ok(None);
        };
        child.try_wait().map_err(AdapterError::InspectSftp)
    }

    pub fn wait_with_output(mut self) -> Result<SftpOutput, AdapterError> {
        let mut child = self.child.take().expect("live SFTP child");
        let result = (|| {
            let stdout = child.stdout.take().ok_or_else(|| {
                AdapterError::InspectSftp(io::Error::other("SFTP stdout pipe is unavailable"))
            })?;
            let stderr = child.stderr.take().ok_or_else(|| {
                AdapterError::InspectSftp(io::Error::other("SFTP stderr pipe is unavailable"))
            })?;
            let stdout_reader = thread::Builder::new()
                .name("localdesk-sftp-stdout".to_owned())
                .spawn(move || read_bounded(stdout, MAX_SFTP_STDOUT_BYTES))
                .map_err(AdapterError::InspectSftp)?;
            let stderr_reader = thread::Builder::new()
                .name("localdesk-sftp-stderr".to_owned())
                .spawn(move || read_bounded(stderr, MAX_SFTP_STDERR_BYTES))
                .map_err(AdapterError::InspectSftp)?;

            let status = child.wait().map_err(AdapterError::InspectSftp)?;
            let stdout = join_bounded_reader(stdout_reader)?;
            let stderr = join_bounded_reader(stderr_reader)?;
            if stdout.dropped_bytes != 0 {
                return Err(AdapterError::SftpOutputLimit {
                    stream: "stdout",
                    maximum: MAX_SFTP_STDOUT_BYTES,
                });
            }
            if stderr.dropped_bytes != 0 {
                return Err(AdapterError::SftpOutputLimit {
                    stream: "stderr",
                    maximum: MAX_SFTP_STDERR_BYTES,
                });
            }
            Ok(SftpOutput {
                status,
                stdout: stdout.bytes,
                stderr: stderr.bytes,
            })
        })();
        if result.is_err() {
            let _ = child.kill();
            let _ = child.wait();
        }
        result
    }

    pub fn close(&mut self) -> Result<ExitStatus, AdapterError> {
        let child = self.child.as_mut().expect("live SFTP child");
        if let Some(status) = child.try_wait().map_err(AdapterError::InspectSftp)? {
            return Ok(status);
        }
        child.kill().map_err(AdapterError::CloseSftp)?;
        child.wait().map_err(AdapterError::CloseSftp)
    }
}

struct BoundedRead {
    bytes: Vec<u8>,
    dropped_bytes: u64,
}

fn read_bounded(mut reader: impl Read, maximum: usize) -> io::Result<BoundedRead> {
    let mut bytes = Vec::with_capacity(maximum.min(8 * 1024));
    let mut dropped_bytes = 0_u64;
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let retained = maximum.saturating_sub(bytes.len()).min(count);
        bytes.extend_from_slice(&buffer[..retained]);
        dropped_bytes = dropped_bytes.saturating_add((count - retained) as u64);
    }
    Ok(BoundedRead {
        bytes,
        dropped_bytes,
    })
}

async fn read_bounded_async(
    mut reader: impl AsyncRead + Unpin,
    maximum: usize,
) -> io::Result<BoundedRead> {
    let mut bytes = Vec::with_capacity(maximum.min(8 * 1024));
    let mut dropped_bytes = 0_u64;
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        let retained = maximum.saturating_sub(bytes.len()).min(count);
        bytes.extend_from_slice(&buffer[..retained]);
        dropped_bytes = dropped_bytes.saturating_add((count - retained) as u64);
    }
    Ok(BoundedRead {
        bytes,
        dropped_bytes,
    })
}

fn join_bounded_reader(
    reader: thread::JoinHandle<io::Result<BoundedRead>>,
) -> Result<BoundedRead, AdapterError> {
    reader
        .join()
        .map_err(|_| AdapterError::InspectSftp(io::Error::other("SFTP reader panicked")))?
        .map_err(AdapterError::InspectSftp)
}

impl Drop for SftpSession {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut()
            && matches!(child.try_wait(), Ok(None))
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn classify_session(
    exit: Option<&ExitStatus>,
    transcript: &[u8],
    close_requested: bool,
) -> SessionState {
    let Some(exit) = exit else {
        return SessionState::Running;
    };
    if close_requested {
        return SessionState::ClosedByClient;
    }
    if exit.code() != Some(255) {
        return SessionState::Exited { code: exit.code() };
    }
    let reason = classify_disconnect_reason(transcript);
    SessionState::Disconnected { reason }
}

fn classify_disconnect_reason(transcript: &[u8]) -> DisconnectReason {
    let output = String::from_utf8_lossy(transcript);
    if output.contains("REMOTE HOST IDENTIFICATION HAS CHANGED") {
        DisconnectReason::HostKeyChanged
    } else if output.contains("REVOKED HOST KEY DETECTED") || output.contains("revoked host key") {
        DisconnectReason::HostKeyRevoked
    } else if output.contains("Host key verification failed")
        || output.contains("No ED25519 host key is known")
        || output.contains("No ECDSA host key is known")
        || output.contains("No RSA host key is known")
    {
        DisconnectReason::HostKeyUnknown
    } else if output.contains("Permission denied") {
        DisconnectReason::AuthenticationFailed
    } else if output.contains("No route to host")
        || output.contains("Network is unreachable")
        || output.contains("Could not resolve hostname")
    {
        DisconnectReason::NetworkUnreachable
    } else if output.contains("Connection reset")
        || output.contains("Connection timed out")
        || output.contains("Connection closed")
        || output.contains("Broken pipe")
    {
        DisconnectReason::ConnectionLost
    } else {
        DisconnectReason::OpenSshFailure
    }
}

struct PreparedConnection {
    config: NamedTempFile,
}

impl PreparedConnection {
    fn new(profile: &SshProfile) -> Result<Self, AdapterError> {
        profile.validate()?;
        let content = render_config(profile);
        Ok(Self {
            config: write_private_file("localdesk-ssh-config", &content)?,
        })
    }

    fn terminal_args(&self) -> Vec<OsString> {
        vec![
            OsString::from("-F"),
            self.config.path().as_os_str().to_owned(),
            OsString::from("-tt"),
            OsString::from(TARGET_ALIAS),
        ]
    }

    fn sftp_args(&self, batch_path: &Path) -> Vec<OsString> {
        vec![
            OsString::from("-S"),
            OsString::from(SSH_PROGRAM),
            OsString::from("-q"),
            OsString::from("-F"),
            self.config.path().as_os_str().to_owned(),
            OsString::from("-b"),
            batch_path.as_os_str().to_owned(),
            OsString::from(TARGET_ALIAS),
        ]
    }

    fn structured_sftp_args(&self) -> Vec<OsString> {
        vec![
            OsString::from("-F"),
            self.config.path().as_os_str().to_owned(),
            OsString::from("-T"),
            OsString::from("-s"),
            OsString::from(TARGET_ALIAS),
            OsString::from("sftp"),
        ]
    }
}

fn fixed_command(program: &'static str, args: Vec<OsString>) -> Command {
    let mut command = Command::new(program);
    command
        .args(args)
        .env("LC_ALL", "C")
        .env_remove("SSH_ASKPASS")
        .env_remove(crate::askpass::ASKPASS_REQUIRE_ENV)
        .env_remove(crate::askpass::ASKPASS_SECRET_ENV);
    command
}

fn tokio_fixed_command(program: &'static str, args: Vec<OsString>) -> TokioCommand {
    let mut command = TokioCommand::new(program);
    command
        .args(args)
        .env("LC_ALL", "C")
        .env_remove("SSH_ASKPASS")
        .env_remove(crate::askpass::ASKPASS_REQUIRE_ENV)
        .env_remove(crate::askpass::ASKPASS_SECRET_ENV);
    command
}

fn render_config(profile: &SshProfile) -> Vec<u8> {
    let mut config = String::new();
    config.push_str("Host *\n");
    config.push_str("  AddKeysToAgent no\n");
    config.push_str("  CanonicalizeHostname no\n");
    config.push_str("  ClearAllForwardings yes\n");
    config.push_str(&format!("  ConnectTimeout {CONNECT_TIMEOUT_SECONDS}\n"));
    config.push_str("  ControlMaster no\n");
    config.push_str("  ControlPath none\n");
    config.push_str("  ControlPersist no\n");
    config.push_str("  EnableEscapeCommandline no\n");
    config.push_str("  EscapeChar none\n");
    config.push_str("  ExitOnForwardFailure yes\n");
    config.push_str("  ForwardAgent no\n");
    config.push_str("  ForwardX11 no\n");
    config.push_str("  HashKnownHosts yes\n");
    config.push_str("  KbdInteractiveAuthentication no\n");
    config.push_str("  PermitLocalCommand no\n");
    config.push_str("  ServerAliveCountMax 3\n");
    config.push_str("  ServerAliveInterval 15\n");
    config.push_str("  TCPKeepAlive yes\n");
    config.push_str("  Tunnel no\n");
    config.push_str("  UpdateHostKeys no\n");
    config.push_str("  VerifyHostKeyDNS no\n\n");

    for (index, endpoint) in profile.jump_hosts.iter().enumerate() {
        render_host(&mut config, &format!("localdesk-jump-{index}"), endpoint);
    }
    render_host(&mut config, TARGET_ALIAS, &profile.target);
    if !profile.jump_hosts.is_empty() {
        let aliases = (0..profile.jump_hosts.len())
            .map(|index| format!("localdesk-jump-{index}"))
            .collect::<Vec<_>>()
            .join(",");
        config.push_str(&format!("  ProxyJump {aliases}\n"));
    }
    config.into_bytes()
}

fn render_host(config: &mut String, alias: &str, endpoint: &Endpoint) {
    config.push_str(&format!("Host {alias}\n"));
    config.push_str(&format!("  HostName {}\n", endpoint.host));
    config.push_str(&format!("  Port {}\n", endpoint.port));
    if let Some(user) = &endpoint.user {
        config.push_str(&format!("  User {user}\n"));
    }
    config.push_str(&format!(
        "  StrictHostKeyChecking {}\n",
        match endpoint.trust.policy {
            HostKeyPolicy::Strict => "yes",
            HostKeyPolicy::AcceptNew => "accept-new",
        }
    ));
    config.push_str(&format!(
        "  UserKnownHostsFile {}\n",
        quote_config_path(&endpoint.trust.known_hosts_file)
    ));
    if let Some(path) = &endpoint.trust.revoked_host_keys_file {
        config.push_str(&format!("  RevokedHostKeys {}\n", quote_config_path(path)));
    }
    match &endpoint.authentication {
        Authentication::Agent => {
            config.push_str("  BatchMode yes\n");
            config.push_str("  IdentityAgent SSH_AUTH_SOCK\n");
            config.push_str("  IdentitiesOnly no\n");
            config.push_str("  NumberOfPasswordPrompts 0\n");
            config.push_str("  PasswordAuthentication no\n");
            config.push_str("  PreferredAuthentications publickey\n");
            config.push_str("  PubkeyAuthentication yes\n");
        }
        Authentication::IdentityFile(path) => {
            config.push_str("  BatchMode yes\n");
            config.push_str("  IdentityAgent none\n");
            config.push_str("  IdentitiesOnly yes\n");
            config.push_str(&format!("  IdentityFile {}\n", quote_config_path(path)));
            config.push_str("  NumberOfPasswordPrompts 0\n");
            config.push_str("  PasswordAuthentication no\n");
            config.push_str("  PreferredAuthentications publickey\n");
            config.push_str("  PubkeyAuthentication yes\n");
        }
        Authentication::IdentityFileWithPassphrase(path) => {
            config.push_str("  BatchMode no\n");
            config.push_str("  IdentityAgent none\n");
            config.push_str("  IdentitiesOnly yes\n");
            config.push_str(&format!("  IdentityFile {}\n", quote_config_path(path)));
            config.push_str("  NumberOfPasswordPrompts 1\n");
            config.push_str("  PasswordAuthentication no\n");
            config.push_str("  PreferredAuthentications publickey\n");
            config.push_str("  PubkeyAuthentication yes\n");
        }
        Authentication::Password => {
            config.push_str("  BatchMode no\n");
            config.push_str("  IdentityAgent none\n");
            config.push_str("  IdentitiesOnly yes\n");
            config.push_str("  NumberOfPasswordPrompts 1\n");
            config.push_str("  PasswordAuthentication yes\n");
            config.push_str("  PreferredAuthentications password\n");
            config.push_str("  PubkeyAuthentication no\n");
        }
    }
    config.push('\n');
}

fn quote_config_path(path: &Path) -> String {
    let value = path.to_str().expect("validated UTF-8 path");
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn render_sftp_batch(operations: &[SftpOperation]) -> Result<Vec<u8>, AdapterError> {
    if operations.is_empty() {
        return Err(AdapterError::EmptySftpBatch);
    }
    let mut batch = String::new();
    for (index, operation) in operations.iter().enumerate() {
        let prefix = format!("operations[{index}]");
        match operation {
            SftpOperation::List { remote_path } => {
                batch.push_str(&format!(
                    "ls -lan {}\n",
                    quote_remote(remote_path, &prefix)?
                ));
            }
            SftpOperation::Stat { remote_path } => {
                let parent = sftp_parent_path(remote_path, &format!("{prefix}.remote_path"))?;
                batch.push_str(&format!(
                    "ls -lan {}\n",
                    quote_remote(&parent, &format!("{prefix}.remote_path"))?
                ));
            }
            SftpOperation::Download {
                remote_path,
                local_path,
            } => batch.push_str(&format!(
                "get {} {}\n",
                quote_remote(remote_path, &format!("{prefix}.remote_path"))?,
                quote_local(local_path, &format!("{prefix}.local_path"))?
            )),
            SftpOperation::Upload {
                local_path,
                remote_path,
            } => batch.push_str(&format!(
                "put {} {}\n",
                quote_local(local_path, &format!("{prefix}.local_path"))?,
                quote_remote(remote_path, &format!("{prefix}.remote_path"))?
            )),
            SftpOperation::CreateDirectory { remote_path } => batch.push_str(&format!(
                "mkdir {}\n",
                quote_remote(remote_path, &format!("{prefix}.remote_path"))?
            )),
            SftpOperation::RemoveFile { remote_path } => batch.push_str(&format!(
                "rm {}\n",
                quote_remote(remote_path, &format!("{prefix}.remote_path"))?
            )),
            SftpOperation::RemoveDirectory { remote_path } => batch.push_str(&format!(
                "rmdir {}\n",
                quote_remote(remote_path, &format!("{prefix}.remote_path"))?
            )),
            SftpOperation::Rename { from, to } => batch.push_str(&format!(
                "rename {} {}\n",
                quote_remote(from, &format!("{prefix}.from"))?,
                quote_remote(to, &format!("{prefix}.to"))?
            )),
        }
    }
    Ok(batch.into_bytes())
}

fn sftp_parent_path(value: &str, field: &str) -> Result<String, AdapterError> {
    if value.is_empty() || value.contains(['\0', '\n', '\r']) {
        return Err(AdapterError::InvalidSftpPath {
            field: field.to_owned(),
        });
    }
    let value = value.trim_end_matches('/');
    if value.is_empty() {
        return Ok("/".to_owned());
    }
    Ok(match value.rsplit_once('/') {
        Some(("", _)) => "/".to_owned(),
        Some((parent, _)) => parent.to_owned(),
        None => ".".to_owned(),
    })
}

fn quote_remote(value: &str, field: &str) -> Result<String, AdapterError> {
    if value.is_empty() || value.contains(['\0', '\n', '\r']) {
        return Err(AdapterError::InvalidSftpPath {
            field: field.to_owned(),
        });
    }
    Ok(quote_sftp(value))
}

fn quote_local(path: &Path, field: &str) -> Result<String, AdapterError> {
    let Some(value) = path.to_str() else {
        return Err(AdapterError::InvalidLocalPath {
            field: field.to_owned(),
        });
    };
    if !path.is_absolute() || value.contains(['\0', '\n', '\r']) {
        return Err(AdapterError::InvalidLocalPath {
            field: field.to_owned(),
        });
    }
    Ok(quote_sftp(value))
}

fn quote_sftp(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn write_private_file(prefix: &str, content: &[u8]) -> Result<NamedTempFile, AdapterError> {
    let mut file = Builder::new()
        .prefix(prefix)
        .tempfile()
        .map_err(AdapterError::CreateInput)?;
    file.write_all(content).map_err(AdapterError::WriteInput)?;
    file.as_file()
        .sync_all()
        .map_err(AdapterError::WriteInput)?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        ffi::OsStr,
        io::Cursor,
        os::unix::{fs::PermissionsExt, process::ExitStatusExt},
        path::PathBuf,
        time::{Duration, Instant},
    };

    fn endpoint(host: &str, known_hosts: &str, policy: HostKeyPolicy) -> Endpoint {
        Endpoint {
            host: host.to_owned(),
            port: 22,
            user: Some("operator".to_owned()),
            trust: crate::HostTrust {
                known_hosts_file: PathBuf::from(known_hosts),
                revoked_host_keys_file: Some(PathBuf::from(format!("{known_hosts}.revoked"))),
                policy,
            },
            authentication: Authentication::Agent,
        }
    }

    fn terminal_fixture(program: &str, arguments: &[&str]) -> TerminalSession {
        let mut command = Command::new(program);
        command.args(arguments);
        TerminalSession {
            pty: PtySession::spawn(&mut command, PtySize::new(24, 80).expect("size"))
                .expect("spawn terminal fixture"),
            pending_output: BoundedPendingOutput::default(),
            transcript: BoundedTranscript::default(),
            _config: write_private_file("localdesk-terminal-test", b"Host *\n")
                .expect("fixture config"),
        }
    }

    fn poll_until_stopped(session: &mut TerminalSession) -> TerminalStatus {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let status = session.poll_state().expect("poll terminal fixture");
            if status.state != SessionState::Running {
                return status;
            }
            assert!(Instant::now() < deadline, "terminal fixture did not stop");
            std::thread::yield_now();
        }
    }

    #[test]
    fn argv_is_fixed_and_contains_no_profile_tokens() {
        let profile = SshProfile {
            target: endpoint(
                "target.example",
                "/tmp/target known_hosts",
                HostKeyPolicy::Strict,
            ),
            jump_hosts: Vec::new(),
        };
        let prepared = PreparedConnection::new(&profile).expect("prepare");
        let terminal = prepared.terminal_args();
        assert_eq!(terminal[0], OsStr::new("-F"));
        assert_eq!(terminal[2], OsStr::new("-tt"));
        assert_eq!(terminal[3], OsStr::new(TARGET_ALIAS));
        assert!(!terminal.iter().any(|arg| arg == "target.example"));

        let sftp = prepared.sftp_args(Path::new("/tmp/batch"));
        assert_eq!(sftp[0], OsStr::new("-S"));
        assert_eq!(sftp[1], OsStr::new(SSH_PROGRAM));
        assert_eq!(sftp[2], OsStr::new("-q"));
        assert_eq!(sftp[3], OsStr::new("-F"));
        assert_eq!(sftp[5], OsStr::new("-b"));
        assert_eq!(sftp[7], OsStr::new(TARGET_ALIAS));

        let structured = prepared.structured_sftp_args();
        assert_eq!(structured[0], OsStr::new("-F"));
        assert_eq!(structured[2], OsStr::new("-T"));
        assert_eq!(structured[3], OsStr::new("-s"));
        assert_eq!(structured[4], OsStr::new(TARGET_ALIAS));
        assert_eq!(structured[5], OsStr::new("sftp"));
        assert!(!structured.iter().any(|arg| arg == "target.example"));
    }

    #[test]
    fn every_jump_has_independent_trust_and_agent_forwarding_is_off() {
        let mut first = endpoint("jump-one.example", "/tmp/jump-one", HostKeyPolicy::Strict);
        first.authentication = Authentication::IdentityFile(PathBuf::from("/tmp/id_jump_one"));
        let profile = SshProfile {
            target: endpoint("target.example", "/tmp/target", HostKeyPolicy::Strict),
            jump_hosts: vec![
                first,
                endpoint(
                    "jump-two.example",
                    "/tmp/jump-two",
                    HostKeyPolicy::AcceptNew,
                ),
            ],
        };
        profile.validate().expect("profile");
        let config = String::from_utf8(render_config(&profile)).expect("UTF-8 config");

        assert!(config.contains("ForwardAgent no"));
        assert!(config.contains("Host localdesk-jump-0\n"));
        assert!(config.contains("UserKnownHostsFile \"/tmp/jump-one\""));
        assert!(config.contains("Host localdesk-jump-1\n"));
        assert!(config.contains("UserKnownHostsFile \"/tmp/jump-two\""));
        assert!(config.contains("StrictHostKeyChecking accept-new"));
        assert!(config.contains("Host localdesk-target\n"));
        assert!(config.contains("UserKnownHostsFile \"/tmp/target\""));
        assert!(config.contains("ProxyJump localdesk-jump-0,localdesk-jump-1"));
        assert!(config.contains("IdentityAgent none"));
        assert!(config.contains("IdentityAgent SSH_AUTH_SOCK"));
        assert_eq!(config.matches("RevokedHostKeys").count(), 3);
    }

    #[test]
    fn password_and_encrypted_key_hosts_allow_only_the_requested_prompt() {
        let mut password = endpoint("password.example", "/tmp/password", HostKeyPolicy::Strict);
        password.authentication = Authentication::Password;
        let password_config = String::from_utf8(render_config(&SshProfile {
            target: password,
            jump_hosts: Vec::new(),
        }))
        .expect("password config");
        assert!(password_config.contains("  BatchMode no\n"));
        assert!(password_config.contains("  NumberOfPasswordPrompts 1\n"));
        assert!(password_config.contains("  PasswordAuthentication yes\n"));
        assert!(password_config.contains("  PreferredAuthentications password\n"));
        assert!(password_config.contains("  PubkeyAuthentication no\n"));

        let mut encrypted = endpoint("key.example", "/tmp/key", HostKeyPolicy::Strict);
        encrypted.authentication =
            Authentication::IdentityFileWithPassphrase(PathBuf::from("/tmp/encrypted-key"));
        let key_config = String::from_utf8(render_config(&SshProfile {
            target: encrypted,
            jump_hosts: Vec::new(),
        }))
        .expect("encrypted key config");
        assert!(key_config.contains("  BatchMode no\n"));
        assert!(key_config.contains("  IdentityFile \"/tmp/encrypted-key\"\n"));
        assert!(key_config.contains("  NumberOfPasswordPrompts 1\n"));
        assert!(key_config.contains("  PasswordAuthentication no\n"));
        assert!(key_config.contains("  PreferredAuthentications publickey\n"));
        assert!(key_config.contains("  PubkeyAuthentication yes\n"));
    }

    #[test]
    fn fixed_commands_remove_inherited_askpass_environment() {
        let command = fixed_command(SSH_PROGRAM, Vec::new());
        let environment = command
            .get_envs()
            .map(|(key, value)| (key.to_owned(), value.map(ToOwned::to_owned)))
            .collect::<std::collections::HashMap<_, _>>();
        for key in [
            OsStr::new("SSH_ASKPASS"),
            OsStr::new(crate::askpass::ASKPASS_REQUIRE_ENV),
            OsStr::new(crate::askpass::ASKPASS_SECRET_ENV),
        ] {
            assert_eq!(environment.get(key), Some(&None));
        }
    }

    #[test]
    fn sftp_batch_is_typed_and_quotes_paths_without_local_shell_commands() {
        let batch = render_sftp_batch(&[
            SftpOperation::List {
                remote_path: "/srv/files".to_owned(),
            },
            SftpOperation::Download {
                remote_path: "/srv/a b.txt".to_owned(),
                local_path: PathBuf::from("/tmp/a b.txt"),
            },
            SftpOperation::Rename {
                from: "/srv/old".to_owned(),
                to: "/srv/new".to_owned(),
            },
            SftpOperation::Stat {
                remote_path: "/srv/new".to_owned(),
            },
        ])
        .expect("batch");
        let batch = String::from_utf8(batch).expect("UTF-8 batch");
        assert_eq!(
            batch,
            "ls -lan \"/srv/files\"\nget \"/srv/a b.txt\" \"/tmp/a b.txt\"\nrename \"/srv/old\" \"/srv/new\"\nls -lan \"/srv\"\n"
        );
        assert!(!batch.lines().any(|line| line.starts_with('!')));
    }

    #[test]
    fn changed_revoked_unknown_auth_and_network_failures_are_typed() {
        let failed = ExitStatus::from_raw(255 << 8);
        let cases = [
            (
                b"WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!".as_slice(),
                DisconnectReason::HostKeyChanged,
            ),
            (
                b"WARNING: REVOKED HOST KEY DETECTED!".as_slice(),
                DisconnectReason::HostKeyRevoked,
            ),
            (
                b"Host key verification failed.".as_slice(),
                DisconnectReason::HostKeyUnknown,
            ),
            (
                b"Permission denied (publickey).".as_slice(),
                DisconnectReason::AuthenticationFailed,
            ),
            (
                b"ssh: connect to host x: Network is unreachable".as_slice(),
                DisconnectReason::NetworkUnreachable,
            ),
        ];
        for (transcript, reason) in cases {
            assert_eq!(
                classify_session(Some(&failed), transcript, false),
                SessionState::Disconnected { reason }
            );
        }
        assert_eq!(
            classify_session(Some(&failed), b"anything", true),
            SessionState::ClosedByClient
        );
    }

    #[test]
    fn malformed_sftp_paths_are_rejected_before_spawn() {
        let error = render_sftp_batch(&[SftpOperation::List {
            remote_path: "ok\n!touch /tmp/escaped".to_owned(),
        }])
        .expect_err("newline must fail");
        assert!(matches!(error, AdapterError::InvalidSftpPath { .. }));

        let error = render_sftp_batch(&[SftpOperation::Upload {
            local_path: PathBuf::from("relative"),
            remote_path: "/srv/file".to_owned(),
        }])
        .expect_err("relative local path must fail");
        assert!(matches!(error, AdapterError::InvalidLocalPath { .. }));
    }

    #[test]
    fn installed_openssh_parses_target_and_each_jump_without_network_access() {
        let profile = SshProfile {
            target: endpoint("target.example", "/tmp/target", HostKeyPolicy::Strict),
            jump_hosts: vec![
                endpoint("jump-one.example", "/tmp/jump-one", HostKeyPolicy::Strict),
                endpoint(
                    "jump-two.example",
                    "/tmp/jump-two",
                    HostKeyPolicy::AcceptNew,
                ),
            ],
        };
        let prepared = PreparedConnection::new(&profile).expect("prepare");
        for alias in [TARGET_ALIAS, "localdesk-jump-0", "localdesk-jump-1"] {
            let output = Command::new(SSH_PROGRAM)
                .args(["-G", "-F"])
                .arg(prepared.config.path())
                .arg(alias)
                .env("LC_ALL", "C")
                .output()
                .expect("run ssh -G");
            assert!(
                output.status.success(),
                "ssh -G rejected {alias}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn generated_openssh_inputs_are_owner_only() {
        let config = write_private_file("localdesk-mode-config", b"Host *\n").expect("config");
        let batch = write_private_file("localdesk-mode-batch", b"ls \"/\"\n").expect("batch");
        for file in [config, batch] {
            let mode = file
                .as_file()
                .metadata()
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn terminal_api_bounds_reads_input_and_debug_output() {
        let mut session = terminal_fixture("/usr/bin/cat", &[]);
        assert_eq!(session.capabilities(), TERMINAL_CAPABILITIES);
        assert!(matches!(
            session.read_output(0),
            Err(TerminalError::InvalidReadLimit { .. })
        ));
        assert!(matches!(
            session.read_output(MAX_TERMINAL_OUTPUT_BYTES + 1),
            Err(TerminalError::InvalidReadLimit { .. })
        ));

        let oversized = vec![0_u8; MAX_TERMINAL_INPUT_BYTES + 1];
        let error = session
            .write_input(&oversized)
            .expect_err("oversized input must fail before writing");
        assert_eq!(error.code(), "terminal_input_too_large");
        assert!(!error.retryable());

        let secret = b"terminal-secret-fixture\n";
        session.write_input(secret).expect("write bounded input");
        let deadline = Instant::now() + Duration::from_secs(2);
        let output = loop {
            match session.read_output(128).expect("bounded read") {
                TerminalRead::Data(output) => break output,
                TerminalRead::Pending => {
                    assert!(Instant::now() < deadline, "terminal output did not arrive");
                    std::thread::yield_now();
                }
                TerminalRead::EndOfStream => panic!("fixture exited before producing output"),
            }
        };
        assert!(output.len() <= 128);
        assert!(!format!("{output:?}").contains("terminal-secret-fixture"));
        assert!(!format!("{session:?}").contains("terminal-secret-fixture"));

        let status = session.close().expect("close and reap terminal fixture");
        assert_eq!(status.state, SessionState::ClosedByClient);
        assert_eq!(
            session.poll_state().expect("poll reaped fixture").state,
            SessionState::ClosedByClient
        );
    }

    #[test]
    fn poll_captures_changed_revoked_and_auth_failures_before_first_read() {
        let cases = [
            (
                "WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!",
                DisconnectReason::HostKeyChanged,
            ),
            (
                "WARNING: REVOKED HOST KEY DETECTED!",
                DisconnectReason::HostKeyRevoked,
            ),
            (
                "Permission denied (publickey).",
                DisconnectReason::AuthenticationFailed,
            ),
        ];

        for (message, reason) in cases {
            let mut session = terminal_fixture(
                "/usr/bin/sh",
                &[
                    "-c",
                    "printf '%s' \"$1\" >&2; exit 255",
                    "terminal-failure-fixture",
                    message,
                ],
            );
            let status = poll_until_stopped(&mut session);
            assert_eq!(status.state, SessionState::Disconnected { reason });
            assert_eq!(status.pending_output_dropped_bytes, 0);
            assert!(status.pending_output_bytes >= message.len());

            let TerminalRead::Data(output) = session
                .read_output(MAX_TERMINAL_OUTPUT_BYTES)
                .expect("read poll-captured terminal output")
            else {
                panic!("poll-captured output must remain readable");
            };
            assert!(String::from_utf8_lossy(output.as_bytes()).contains(message));
        }
    }

    #[test]
    fn pending_terminal_output_retains_only_the_newest_bounded_bytes() {
        let mut pending = BoundedPendingOutput::default();
        pending.push(&vec![b'a'; MAX_TERMINAL_OUTPUT_BYTES]);
        pending.push(b"tail");
        assert_eq!(pending.len(), MAX_TERMINAL_OUTPUT_BYTES);
        assert_eq!(pending.dropped_bytes(), 4);
        let retained = pending
            .take(MAX_TERMINAL_OUTPUT_BYTES)
            .expect("retained pending output");
        assert_eq!(&retained[MAX_TERMINAL_OUTPUT_BYTES - 4..], b"tail");
    }

    #[test]
    fn repeated_poll_captures_failure_after_more_than_one_bounded_budget() {
        let message = "Permission denied (publickey).";
        let mut session = terminal_fixture(
            "/usr/bin/sh",
            &[
                "-c",
                "printf '%070000d' 0; printf '%s' \"$1\" >&2; exit 255",
                "terminal-large-failure-fixture",
                message,
            ],
        );

        let status = poll_until_stopped(&mut session);
        assert_eq!(
            status.state,
            SessionState::Disconnected {
                reason: DisconnectReason::AuthenticationFailed,
            }
        );
        assert_eq!(status.pending_output_bytes, MAX_TERMINAL_OUTPUT_BYTES);
        assert!(status.pending_output_dropped_bytes > 0);
        assert_eq!(
            status.transcript_retained_bytes,
            MAX_TERMINAL_TRANSCRIPT_BYTES
        );
        assert!(status.transcript_dropped_bytes > 0);

        let TerminalRead::Data(output) = session
            .read_output(MAX_TERMINAL_OUTPUT_BYTES)
            .expect("read bounded poll capture")
        else {
            panic!("bounded poll capture must remain readable");
        };
        assert!(String::from_utf8_lossy(output.as_bytes()).contains(message));
    }

    #[test]
    fn terminal_transcript_keeps_only_the_newest_bounded_bytes() {
        let mut transcript = BoundedTranscript::default();
        transcript.push(&vec![b'a'; MAX_TERMINAL_TRANSCRIPT_BYTES]);
        transcript.push(b"tail");
        assert_eq!(transcript.as_bytes().len(), MAX_TERMINAL_TRANSCRIPT_BYTES);
        assert_eq!(transcript.dropped_bytes(), 4);
        assert_eq!(
            &transcript.as_bytes()[MAX_TERMINAL_TRANSCRIPT_BYTES - 4..],
            b"tail"
        );
    }

    #[test]
    fn bounded_sftp_capture_drains_but_does_not_retain_excess_output() {
        let captured = read_bounded(Cursor::new(b"0123456789"), 4).expect("bounded capture");
        assert_eq!(captured.bytes, b"0123");
        assert_eq!(captured.dropped_bytes, 6);

        let output = SftpOutput {
            status: ExitStatus::from_raw(0),
            stdout: b"remote-secret-name".to_vec(),
            stderr: b"private-error".to_vec(),
        };
        let debug = format!("{output:?}");
        assert!(!debug.contains("remote-secret-name"));
        assert!(!debug.contains("private-error"));
    }
}
