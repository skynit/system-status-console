use crate::{
    AdapterAvailability, CapabilityMatrix, MAX_REMOTE_CHUNK_BYTES, ObjectIdentity,
    RemoteConnectionProfile, RemoteEntry, RemoteError, RemotePath, RemoteProtocol, RemoteSession,
    SecretStore, SessionId,
};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};
use uuid::Uuid;

pub type AdapterFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum RemoteIoControlSupport {
    Supported,
    Unsupported(crate::SafeReason),
}

impl RemoteIoControlSupport {
    pub const fn is_supported(&self) -> bool {
        matches!(self, Self::Supported)
    }
}

#[derive(Clone)]
pub struct RemoteIoControl {
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
}

impl fmt::Debug for RemoteIoControl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteIoControl")
            .field("deadline", &self.deadline)
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

impl RemoteIoControl {
    pub fn new(deadline: Instant) -> Self {
        Self {
            deadline,
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub const fn deadline(&self) -> Instant {
        self.deadline
    }

    pub fn with_deadline(&self, deadline: Instant) -> Self {
        Self {
            deadline: deadline.min(self.deadline),
            cancelled: self.cancelled.clone(),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub fn check(&self, operation: crate::RemoteOperation) -> Result<(), RemoteError> {
        if self.is_cancelled() {
            return Err(RemoteError::new(
                crate::RemoteErrorKind::Cancelled,
                operation,
                crate::SafeReason::new("remote_io_cancelled").expect("static reason is valid"),
                crate::RetryDisposition::Never,
            ));
        }
        if Instant::now() >= self.deadline {
            return Err(RemoteError::new(
                crate::RemoteErrorKind::Timeout,
                operation,
                crate::SafeReason::new("remote_io_deadline_elapsed")
                    .expect("static reason is valid"),
                crate::RetryDisposition::Backoff,
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteReadRequest {
    pub path: RemotePath,
    pub offset: u64,
    pub max_bytes: u32,
    pub expected_identity: Option<ObjectIdentity>,
}

impl RemoteReadRequest {
    pub fn is_bounded(&self) -> bool {
        self.max_bytes > 0 && self.max_bytes <= MAX_REMOTE_CHUNK_BYTES
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteReadChunk {
    pub offset: u64,
    pub bytes: Vec<u8>,
    pub eof: bool,
    pub identity: ObjectIdentity,
}

impl fmt::Debug for RemoteReadChunk {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteReadChunk")
            .field("offset", &self.offset)
            .field("byte_count", &self.bytes.len())
            .field("bytes", &"<redacted>")
            .field("eof", &self.eof)
            .field("identity", &self.identity)
            .finish()
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct BeginWriteRequest {
    pub final_path: RemotePath,
    pub temporary_path: RemotePath,
    pub expected_size_bytes: Option<u64>,
    pub resume_from: Option<u64>,
    pub expected_destination: Option<ObjectIdentity>,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct RemoteWriteHandle(Uuid);

impl RemoteWriteHandle {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub const fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }
}

impl Default for RemoteWriteHandle {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteWriteReceipt {
    pub handle: RemoteWriteHandle,
    pub next_offset: u64,
    pub identity: Option<ObjectIdentity>,
}

pub trait RemoteFileAdapter: Send + Sync {
    fn protocol(&self) -> RemoteProtocol;
    fn availability(&self) -> AdapterAvailability;
    fn capabilities(&self) -> &CapabilityMatrix;

    fn io_control_support(&self) -> RemoteIoControlSupport {
        RemoteIoControlSupport::Unsupported(
            crate::SafeReason::new("remote_io_control_not_supported")
                .expect("static reason is valid"),
        )
    }

    fn connect<'a>(
        &'a self,
        profile: &'a RemoteConnectionProfile,
        secrets: &'a dyn SecretStore,
    ) -> AdapterFuture<'a, Result<Box<dyn RemoteFileSession>, RemoteError>>;

    fn connect_controlled<'a>(
        &'a self,
        _profile: &'a RemoteConnectionProfile,
        _secrets: &'a dyn SecretStore,
        _control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<Box<dyn RemoteFileSession>, RemoteError>> {
        Box::pin(async { Err(io_control_unsupported(crate::RemoteOperation::Connect)) })
    }
}

pub trait RemoteFileSession: Send + Sync {
    fn id(&self) -> SessionId;
    fn snapshot(&self) -> RemoteSession;

    fn io_control_support(&self) -> RemoteIoControlSupport {
        RemoteIoControlSupport::Unsupported(
            crate::SafeReason::new("remote_io_control_not_supported")
                .expect("static reason is valid"),
        )
    }

    fn list<'a>(
        &'a self,
        path: &'a RemotePath,
    ) -> AdapterFuture<'a, Result<Vec<RemoteEntry>, RemoteError>>;

    fn stat<'a>(
        &'a self,
        path: &'a RemotePath,
    ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>>;

    fn create_directory<'a>(
        &'a self,
        path: &'a RemotePath,
    ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>>;

    fn rename<'a>(
        &'a self,
        from: &'a RemotePath,
        to: &'a RemotePath,
    ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>>;

    fn delete<'a>(&'a self, path: &'a RemotePath) -> AdapterFuture<'a, Result<(), RemoteError>>;

    fn read_chunk<'a>(
        &'a self,
        request: RemoteReadRequest,
    ) -> AdapterFuture<'a, Result<RemoteReadChunk, RemoteError>>;

    fn begin_write<'a>(
        &'a self,
        request: BeginWriteRequest,
    ) -> AdapterFuture<'a, Result<RemoteWriteReceipt, RemoteError>>;

    fn write_chunk<'a>(
        &'a self,
        handle: RemoteWriteHandle,
        offset: u64,
        bytes: Vec<u8>,
    ) -> AdapterFuture<'a, Result<RemoteWriteReceipt, RemoteError>>;

    fn commit_write<'a>(
        &'a self,
        handle: RemoteWriteHandle,
        expected_identity: Option<ObjectIdentity>,
    ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>>;

    fn abort_write<'a>(
        &'a self,
        handle: RemoteWriteHandle,
    ) -> AdapterFuture<'a, Result<(), RemoteError>>;

    fn read_chunk_controlled<'a>(
        &'a self,
        _request: RemoteReadRequest,
        _control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<RemoteReadChunk, RemoteError>> {
        Box::pin(async { Err(io_control_unsupported(crate::RemoteOperation::Read)) })
    }

    fn begin_write_controlled<'a>(
        &'a self,
        _request: BeginWriteRequest,
        _control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<RemoteWriteReceipt, RemoteError>> {
        Box::pin(async { Err(io_control_unsupported(crate::RemoteOperation::Write)) })
    }

    fn write_chunk_controlled<'a>(
        &'a self,
        _handle: RemoteWriteHandle,
        _offset: u64,
        _bytes: Vec<u8>,
        _control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<RemoteWriteReceipt, RemoteError>> {
        Box::pin(async { Err(io_control_unsupported(crate::RemoteOperation::Write)) })
    }

    fn commit_write_controlled<'a>(
        &'a self,
        _handle: RemoteWriteHandle,
        _expected_identity: Option<ObjectIdentity>,
        _control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>> {
        Box::pin(async { Err(io_control_unsupported(crate::RemoteOperation::Write)) })
    }

    fn abort_write_controlled<'a>(
        &'a self,
        _handle: RemoteWriteHandle,
        _control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<(), RemoteError>> {
        Box::pin(async { Err(io_control_unsupported(crate::RemoteOperation::Write)) })
    }

    fn disconnect_controlled<'a>(
        &'a self,
        _control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<(), RemoteError>> {
        Box::pin(async { Err(io_control_unsupported(crate::RemoteOperation::Disconnect)) })
    }

    fn disconnect<'a>(&'a self) -> AdapterFuture<'a, Result<(), RemoteError>>;
}

fn io_control_unsupported(operation: crate::RemoteOperation) -> RemoteError {
    RemoteError::new(
        crate::RemoteErrorKind::Unsupported,
        operation,
        crate::SafeReason::new("remote_io_control_not_supported").expect("static reason is valid"),
        crate::RetryDisposition::Never,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RemoteErrorKind, RemoteOperation};

    #[test]
    fn io_control_cancellation_is_shared_across_derived_deadlines() {
        let control = RemoteIoControl::new(Instant::now() + std::time::Duration::from_secs(5));
        let child = control.with_deadline(Instant::now() + std::time::Duration::from_secs(1));

        child.cancel();

        let error = control.check(RemoteOperation::Read).expect_err("cancelled");
        assert_eq!(error.kind, RemoteErrorKind::Cancelled);
        assert_eq!(error.reason.as_str(), "remote_io_cancelled");
    }

    #[test]
    fn io_control_reports_an_elapsed_deadline_as_typed_timeout() {
        let control = RemoteIoControl::new(Instant::now());

        let error = control.check(RemoteOperation::Write).expect_err("elapsed");
        assert_eq!(error.kind, RemoteErrorKind::Timeout);
        assert_eq!(error.reason.as_str(), "remote_io_deadline_elapsed");
        assert_eq!(error.retry, crate::RetryDisposition::Backoff);
    }

    #[test]
    fn default_controlled_io_failure_is_explicitly_unsupported() {
        let error = io_control_unsupported(RemoteOperation::Disconnect);

        assert_eq!(error.kind, RemoteErrorKind::Unsupported);
        assert_eq!(error.reason.as_str(), "remote_io_control_not_supported");
        assert_eq!(error.retry, crate::RetryDisposition::Never);
    }
}
