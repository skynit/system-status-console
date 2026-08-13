use crate::{
    ConflictPolicy, LocalFileHandle, QueueError, ResumeValidation, RunToken, TransferCheckpoint,
    TransferCompletion, TransferDirection, TransferEndpoint, TransferFailure, TransferId,
    TransferMutationError, TransferMutationResult, TransferPage, TransferQuery, TransferQueue,
    TransferStateKind, TransferStore, TransferTask, VerificationLevel,
};
use localdesk_remote_core::{
    AdapterFuture, BeginWriteRequest, CapabilityStatus, FileOperation, MAX_REMOTE_CHUNK_BYTES,
    ObjectIdentity, ProfileId, RemoteError, RemoteErrorKind, RemoteFileSession, RemoteIoControl,
    RemoteIoControlSupport, RemoteOperation, RemotePath, RemoteProtocol, RemoteReadRequest,
    RetryDisposition, SafeReason,
};
use nix::{
    fcntl::{AT_FDCWD, RenameFlags, renameat2},
    libc,
    unistd::Uid,
};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use tokio::{sync::Semaphore, task::JoinSet};
use uuid::Uuid;

pub const DEFAULT_TRANSFER_CHUNK_BYTES: u32 = 256 * 1024;
pub const DEFAULT_EXECUTOR_CONCURRENCY: usize = 4;
const MAX_OPERATION_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MAX_TOTAL_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct ExecutorLimits {
    pub max_concurrent: usize,
    pub chunk_bytes: u32,
    pub operation_timeout: Duration,
    pub total_timeout: Duration,
}

impl ExecutorLimits {
    pub fn new(
        max_concurrent: usize,
        chunk_bytes: u32,
        operation_timeout: Duration,
        total_timeout: Duration,
    ) -> Result<Self, ExecutorLimitsError> {
        if max_concurrent == 0 || max_concurrent > crate::MAX_CONCURRENT_TRANSFERS {
            return Err(ExecutorLimitsError::InvalidConcurrency);
        }
        if chunk_bytes == 0 || chunk_bytes > MAX_REMOTE_CHUNK_BYTES {
            return Err(ExecutorLimitsError::InvalidChunkSize);
        }
        if operation_timeout.is_zero()
            || operation_timeout > MAX_OPERATION_TIMEOUT
            || total_timeout < operation_timeout
            || total_timeout > MAX_TOTAL_TIMEOUT
        {
            return Err(ExecutorLimitsError::InvalidTimeout);
        }
        Ok(Self {
            max_concurrent,
            chunk_bytes,
            operation_timeout,
            total_timeout,
        })
    }
}

impl Default for ExecutorLimits {
    fn default() -> Self {
        Self {
            max_concurrent: DEFAULT_EXECUTOR_CONCURRENCY,
            chunk_bytes: DEFAULT_TRANSFER_CHUNK_BYTES,
            operation_timeout: Duration::from_secs(30),
            total_timeout: Duration::from_secs(60 * 60),
        }
    }
}

#[derive(Debug, Clone, Copy, Error, Eq, PartialEq)]
pub enum ExecutorLimitsError {
    #[error("executor concurrency is outside the hard bound")]
    InvalidConcurrency,
    #[error("executor chunk size is outside the adapter hard bound")]
    InvalidChunkSize,
    #[error("executor timeouts are zero, unordered, or outside hard bounds")]
    InvalidTimeout,
}

pub trait RemoteSessionFactory: Send + Sync {
    fn io_control_support(
        &self,
        endpoint: &crate::RemoteTransferEndpoint,
    ) -> RemoteIoControlSupport;

    fn open<'a>(
        &'a self,
        endpoint: &'a crate::RemoteTransferEndpoint,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<Box<dyn RemoteFileSession>, RemoteError>>;
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum LocalHandleAccess {
    ReadSource,
    StagedDestination,
}

#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq)]
pub struct LocalWriteHandle(Uuid);

impl LocalWriteHandle {
    const fn from_local(handle: LocalFileHandle) -> Self {
        Self(handle.as_uuid())
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct LocalReadChunk {
    pub offset: u64,
    pub bytes: Vec<u8>,
    pub eof: bool,
    pub identity: ObjectIdentity,
}

impl std::fmt::Debug for LocalReadChunk {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalReadChunk")
            .field("offset", &self.offset)
            .field("byte_count", &self.bytes.len())
            .field("bytes", &"<redacted>")
            .field("eof", &self.eof)
            .field("identity", &self.identity)
            .finish()
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct LocalWriteReceipt {
    pub handle: LocalWriteHandle,
    pub next_offset: u64,
    pub identity: Option<ObjectIdentity>,
}

pub trait LocalFileOwner: Send + Sync {
    fn validate_handle(
        &self,
        handle: LocalFileHandle,
        access: LocalHandleAccess,
    ) -> Result<(), RemoteError>;

    fn source_identity<'a>(
        &'a self,
        handle: LocalFileHandle,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<ObjectIdentity, RemoteError>>;

    fn read_chunk<'a>(
        &'a self,
        handle: LocalFileHandle,
        offset: u64,
        max_bytes: u32,
        expected_identity: Option<ObjectIdentity>,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<LocalReadChunk, RemoteError>>;

    fn begin_staged_write<'a>(
        &'a self,
        handle: LocalFileHandle,
        expected_size_bytes: Option<u64>,
        resume_from: Option<u64>,
        expected_destination: Option<ObjectIdentity>,
        conflict_policy: ConflictPolicy,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<LocalWriteReceipt, RemoteError>>;

    fn write_chunk<'a>(
        &'a self,
        handle: LocalWriteHandle,
        offset: u64,
        bytes: Vec<u8>,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<LocalWriteReceipt, RemoteError>>;

    fn commit_staged_write<'a>(
        &'a self,
        handle: LocalWriteHandle,
        expected_size_bytes: Option<u64>,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<ObjectIdentity, RemoteError>>;

    fn abort_staged_write<'a>(
        &'a self,
        handle: LocalWriteHandle,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<(), RemoteError>>;
}

#[derive(Clone)]
pub struct LocalHandleOwner {
    bindings: Arc<Mutex<HashMap<LocalFileHandle, LocalBinding>>>,
    staged: Arc<Mutex<HashMap<LocalWriteHandle, StagedWrite>>>,
    uid: u32,
}

#[derive(Clone)]
struct LocalBinding {
    path: PathBuf,
    access: LocalHandleAccess,
}

#[derive(Clone)]
struct StagedWrite {
    staged_path: PathBuf,
    final_path: PathBuf,
    conflict_policy: ConflictPolicy,
}

impl std::fmt::Debug for LocalHandleOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalHandleOwner")
            .finish_non_exhaustive()
    }
}

impl Default for LocalHandleOwner {
    fn default() -> Self {
        Self::new(Uid::current().as_raw())
    }
}

impl LocalHandleOwner {
    pub fn new(uid: u32) -> Self {
        Self {
            bindings: Arc::new(Mutex::new(HashMap::new())),
            staged: Arc::new(Mutex::new(HashMap::new())),
            uid,
        }
    }

    pub fn bind_source(
        &self,
        handle: LocalFileHandle,
        path: impl Into<PathBuf>,
    ) -> Result<(), RemoteError> {
        let path = path.into();
        open_source_file(&path, RemoteOperation::Read)?;
        self.bind(handle, path, LocalHandleAccess::ReadSource)
    }

    pub fn bind_destination(
        &self,
        handle: LocalFileHandle,
        path: impl Into<PathBuf>,
    ) -> Result<(), RemoteError> {
        let path = path.into();
        validate_destination(&path, self.uid)?;
        self.bind(handle, path, LocalHandleAccess::StagedDestination)
    }

    pub fn unbind(&self, handle: LocalFileHandle) -> Result<bool, RemoteError> {
        Ok(self
            .bindings
            .lock()
            .map_err(|_| owner_poisoned(RemoteOperation::Disconnect))?
            .remove(&handle)
            .is_some())
    }

    fn bind(
        &self,
        handle: LocalFileHandle,
        path: PathBuf,
        access: LocalHandleAccess,
    ) -> Result<(), RemoteError> {
        if !path.is_absolute() {
            return Err(error(
                RemoteErrorKind::InvalidInput,
                RemoteOperation::Stat,
                "local_owner_path_must_be_absolute",
                RetryDisposition::Never,
            ));
        }
        let mut bindings = self
            .bindings
            .lock()
            .map_err(|_| owner_poisoned(RemoteOperation::Stat))?;
        if bindings.contains_key(&handle) {
            return Err(error(
                RemoteErrorKind::Conflict,
                RemoteOperation::Stat,
                "local_handle_already_bound",
                RetryDisposition::UserAction,
            ));
        }
        bindings.insert(handle, LocalBinding { path, access });
        Ok(())
    }

    fn path(
        &self,
        handle: LocalFileHandle,
        access: LocalHandleAccess,
        operation: RemoteOperation,
    ) -> Result<PathBuf, RemoteError> {
        let bindings = self
            .bindings
            .lock()
            .map_err(|_| owner_poisoned(operation))?;
        let binding = bindings.get(&handle).ok_or_else(|| {
            error(
                RemoteErrorKind::NotFound,
                operation,
                "local_handle_not_bound",
                RetryDisposition::UserAction,
            )
        })?;
        if binding.access != access {
            return Err(error(
                RemoteErrorKind::PermissionDenied,
                operation,
                "local_handle_access_mismatch",
                RetryDisposition::Never,
            ));
        }
        Ok(binding.path.clone())
    }
}

impl LocalFileOwner for LocalHandleOwner {
    fn validate_handle(
        &self,
        handle: LocalFileHandle,
        access: LocalHandleAccess,
    ) -> Result<(), RemoteError> {
        let operation = match access {
            LocalHandleAccess::ReadSource => RemoteOperation::Read,
            LocalHandleAccess::StagedDestination => RemoteOperation::Write,
        };
        self.path(handle, access, operation).map(|_| ())
    }

    fn source_identity<'a>(
        &'a self,
        handle: LocalFileHandle,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<ObjectIdentity, RemoteError>> {
        Box::pin(async move {
            control.check(RemoteOperation::Stat)?;
            let path = self.path(handle, LocalHandleAccess::ReadSource, RemoteOperation::Stat)?;
            open_source_file(&path, RemoteOperation::Stat).map(|(_, identity)| identity)
        })
    }

    fn read_chunk<'a>(
        &'a self,
        handle: LocalFileHandle,
        offset: u64,
        max_bytes: u32,
        expected_identity: Option<ObjectIdentity>,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<LocalReadChunk, RemoteError>> {
        Box::pin(async move {
            control.check(RemoteOperation::Read)?;
            if max_bytes == 0 || max_bytes > MAX_REMOTE_CHUNK_BYTES {
                return Err(invalid(RemoteOperation::Read, "local_read_chunk_unbounded"));
            }
            let path = self.path(handle, LocalHandleAccess::ReadSource, RemoteOperation::Read)?;
            let (mut file, identity) = open_source_file(&path, RemoteOperation::Read)?;
            ensure_identity(
                expected_identity.as_ref(),
                Some(&identity),
                RemoteOperation::Read,
            )?;
            file.seek(SeekFrom::Start(offset))
                .map_err(|_| io_error(RemoteOperation::Read, "local_source_seek_failed"))?;
            let mut bytes = vec![0_u8; max_bytes as usize];
            let count = file
                .read(&mut bytes)
                .map_err(|_| io_error(RemoteOperation::Read, "local_source_read_failed"))?;
            control.check(RemoteOperation::Read)?;
            bytes.truncate(count);
            let next = offset.saturating_add(count as u64);
            Ok(LocalReadChunk {
                offset,
                bytes,
                eof: identity.size_bytes.is_some_and(|size| next >= size),
                identity,
            })
        })
    }

    fn begin_staged_write<'a>(
        &'a self,
        handle: LocalFileHandle,
        expected_size_bytes: Option<u64>,
        resume_from: Option<u64>,
        expected_destination: Option<ObjectIdentity>,
        conflict_policy: ConflictPolicy,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<LocalWriteReceipt, RemoteError>> {
        Box::pin(async move {
            control.check(RemoteOperation::Write)?;
            let bound = self.path(
                handle,
                LocalHandleAccess::StagedDestination,
                RemoteOperation::Write,
            )?;
            validate_destination(&bound, self.uid)?;
            let current = optional_identity(&bound, self.uid, RemoteOperation::Write)?;
            ensure_identity(
                expected_destination.as_ref(),
                current.as_ref(),
                RemoteOperation::Write,
            )?;
            let final_path = if current.is_some() {
                match conflict_policy {
                    ConflictPolicy::Fail => {
                        return Err(error(
                            RemoteErrorKind::Conflict,
                            RemoteOperation::Write,
                            "local_destination_exists",
                            RetryDisposition::UserAction,
                        ));
                    }
                    ConflictPolicy::Rename => renamed_path(&bound, handle),
                    ConflictPolicy::Overwrite | ConflictPolicy::Resume => bound,
                }
            } else {
                bound
            };
            let staged_path = staged_path(&final_path, handle)?;
            let offset = resume_from.unwrap_or(0);
            if expected_size_bytes.is_some_and(|size| offset > size) {
                return Err(error(
                    RemoteErrorKind::Conflict,
                    RemoteOperation::Resume,
                    "local_resume_offset_exceeds_size",
                    RetryDisposition::UserAction,
                ));
            }
            let file = open_file(
                &staged_path,
                true,
                true,
                resume_from.is_none(),
                RemoteOperation::Write,
            )?;
            if resume_from.is_some() && file.metadata().map_err(|_| stat_error())?.len() != offset {
                return Err(error(
                    RemoteErrorKind::Conflict,
                    RemoteOperation::Resume,
                    "local_partial_size_changed",
                    RetryDisposition::UserAction,
                ));
            }
            let write_handle = LocalWriteHandle::from_local(handle);
            self.staged
                .lock()
                .map_err(|_| owner_poisoned(RemoteOperation::Write))?
                .insert(
                    write_handle,
                    StagedWrite {
                        staged_path,
                        final_path,
                        conflict_policy,
                    },
                );
            Ok(LocalWriteReceipt {
                handle: write_handle,
                next_offset: offset,
                identity: current,
            })
        })
    }

    fn write_chunk<'a>(
        &'a self,
        handle: LocalWriteHandle,
        offset: u64,
        bytes: Vec<u8>,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<LocalWriteReceipt, RemoteError>> {
        Box::pin(async move {
            control.check(RemoteOperation::Write)?;
            if bytes.is_empty() || bytes.len() > MAX_REMOTE_CHUNK_BYTES as usize {
                return Err(invalid(
                    RemoteOperation::Write,
                    "local_write_chunk_invalid_size",
                ));
            }
            let staged = self
                .staged
                .lock()
                .map_err(|_| owner_poisoned(RemoteOperation::Write))?
                .get(&handle)
                .cloned()
                .ok_or_else(missing_write_handle)?;
            let mut file = open_file(
                &staged.staged_path,
                true,
                false,
                false,
                RemoteOperation::Write,
            )?;
            if file.metadata().map_err(|_| stat_error())?.len() != offset {
                return Err(error(
                    RemoteErrorKind::Conflict,
                    RemoteOperation::Write,
                    "local_write_offset_mismatch",
                    RetryDisposition::UserAction,
                ));
            }
            file.seek(SeekFrom::Start(offset))
                .and_then(|_| file.write_all(&bytes))
                .and_then(|_| file.sync_data())
                .map_err(|_| io_error(RemoteOperation::Write, "local_staged_write_failed"))?;
            control.check(RemoteOperation::Write)?;
            Ok(LocalWriteReceipt {
                handle,
                next_offset: offset.saturating_add(bytes.len() as u64),
                identity: None,
            })
        })
    }

    fn commit_staged_write<'a>(
        &'a self,
        handle: LocalWriteHandle,
        expected_size_bytes: Option<u64>,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<ObjectIdentity, RemoteError>> {
        Box::pin(async move {
            control.check(RemoteOperation::Write)?;
            let staged = self
                .staged
                .lock()
                .map_err(|_| owner_poisoned(RemoteOperation::Write))?
                .get(&handle)
                .cloned()
                .ok_or_else(missing_write_handle)?;
            let size = fs::symlink_metadata(&staged.staged_path)
                .map_err(|_| stat_error())?
                .len();
            if expected_size_bytes.is_some_and(|expected| size != expected) {
                return Err(error(
                    RemoteErrorKind::Conflict,
                    RemoteOperation::Write,
                    "local_staged_size_mismatch",
                    RetryDisposition::UserAction,
                ));
            }
            match staged.conflict_policy {
                ConflictPolicy::Fail => renameat2(
                    AT_FDCWD,
                    &staged.staged_path,
                    AT_FDCWD,
                    &staged.final_path,
                    RenameFlags::RENAME_NOREPLACE,
                )
                .map_err(|_| {
                    error(
                        RemoteErrorKind::Conflict,
                        RemoteOperation::Write,
                        "local_destination_changed_before_commit",
                        RetryDisposition::UserAction,
                    )
                })?,
                _ => fs::rename(&staged.staged_path, &staged.final_path)
                    .map_err(|_| io_error(RemoteOperation::Write, "local_staged_commit_failed"))?,
            }
            control.check(RemoteOperation::Write)?;
            self.staged
                .lock()
                .map_err(|_| owner_poisoned(RemoteOperation::Write))?
                .remove(&handle);
            validate_owned_file(&staged.final_path, self.uid, RemoteOperation::Write)
        })
    }

    fn abort_staged_write<'a>(
        &'a self,
        handle: LocalWriteHandle,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<(), RemoteError>> {
        Box::pin(async move {
            control.check(RemoteOperation::Write)?;
            let staged = self
                .staged
                .lock()
                .map_err(|_| owner_poisoned(RemoteOperation::Write))?
                .remove(&handle);
            if let Some(staged) = staged {
                match fs::remove_file(staged.staged_path) {
                    Ok(()) => {}
                    Err(value) if value.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => {
                        return Err(io_error(
                            RemoteOperation::Write,
                            "local_staged_abort_failed",
                        ));
                    }
                }
            }
            Ok(())
        })
    }
}

pub struct TransferExecutor<S> {
    queue: Arc<Mutex<TransferQueue<S>>>,
    local: Arc<dyn LocalFileOwner>,
    remote: Arc<dyn RemoteSessionFactory>,
    limits: ExecutorLimits,
    permits: Arc<Semaphore>,
    active: Arc<Mutex<HashMap<TransferId, RemoteIoControl>>>,
}

impl<S> Clone for TransferExecutor<S> {
    fn clone(&self) -> Self {
        Self {
            queue: self.queue.clone(),
            local: self.local.clone(),
            remote: self.remote.clone(),
            limits: self.limits,
            permits: self.permits.clone(),
            active: self.active.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct TransferRunOutcome {
    pub task_id: TransferId,
    pub state: TransferStateKind,
}

impl<S: TransferStore + 'static> TransferExecutor<S> {
    pub fn new(
        queue: TransferQueue<S>,
        local: Arc<dyn LocalFileOwner>,
        remote: Arc<dyn RemoteSessionFactory>,
        limits: ExecutorLimits,
    ) -> Self {
        Self {
            queue: Arc::new(Mutex::new(queue)),
            local,
            remote,
            limits,
            permits: Arc::new(Semaphore::new(limits.max_concurrent)),
            active: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn queue(&self) -> Arc<Mutex<TransferQueue<S>>> {
        self.queue.clone()
    }

    pub fn enqueue_public(&self, task: TransferTask) -> Result<TransferTask, ExecutorError> {
        self.ensure_open()?;
        task.validate_public().map_err(QueueError::from)?;
        let access = match task.direction {
            TransferDirection::Upload => LocalHandleAccess::ReadSource,
            TransferDirection::Download => LocalHandleAccess::StagedDestination,
        };
        self.local.validate_handle(local_handle(&task), access)?;
        let mut queue = self
            .queue
            .lock()
            .map_err(|_| ExecutorError::QueuePoisoned)?;
        queue.enqueue(task.clone())?;
        Ok(task)
    }

    pub fn get_public(&self, id: TransferId) -> Result<TransferTask, ExecutorError> {
        self.ensure_open()?;
        let task = self
            .queue
            .lock()
            .map_err(|_| ExecutorError::QueuePoisoned)?
            .task(id)
            .cloned()
            .ok_or(QueueError::TaskNotFound)?;
        task.validate_public().map_err(QueueError::from)?;
        Ok(task)
    }

    pub fn list_public(&self, query: TransferQuery) -> Result<TransferPage, ExecutorError> {
        self.ensure_open()?;
        self.queue
            .lock()
            .map_err(|_| ExecutorError::QueuePoisoned)?
            .page(query)
            .map_err(Into::into)
    }

    pub fn references_profile(&self, profile_id: ProfileId) -> Result<bool, ExecutorError> {
        self.ensure_open()?;
        Ok(self
            .queue
            .lock()
            .map_err(|_| ExecutorError::QueuePoisoned)?
            .tasks()
            .any(|task| task.remote_profile_id() == profile_id))
    }

    pub fn referenced_profiles(&self) -> Result<HashSet<ProfileId>, ExecutorError> {
        Ok(self
            .queue
            .lock()
            .map_err(|_| ExecutorError::QueuePoisoned)?
            .tasks()
            .map(TransferTask::remote_profile_id)
            .collect())
    }

    pub fn request_cancel_public(
        &self,
        id: TransferId,
        expected_revision: u64,
    ) -> Result<TransferMutationResult, ExecutorError> {
        self.ensure_open()?;
        let result = self.mutate_public(id, expected_revision, |task| {
            task.request_cancel(unix_time_ms())
        })?;
        if matches!(result, TransferMutationResult::Updated { .. })
            && let Some(control) = self
                .active
                .lock()
                .map_err(|_| ExecutorError::QueuePoisoned)?
                .get(&id)
        {
            control.cancel();
        }
        Ok(result)
    }

    pub fn request_retry_public(
        &self,
        id: TransferId,
        expected_revision: u64,
    ) -> Result<TransferMutationResult, ExecutorError> {
        self.ensure_open()?;
        self.mutate_public(id, expected_revision, |task| {
            task.schedule_retry(unix_time_ms())
        })
    }

    pub fn resolve_conflict_public(
        &self,
        id: TransferId,
        expected_revision: u64,
        policy: ConflictPolicy,
    ) -> Result<TransferMutationResult, ExecutorError> {
        self.ensure_open()?;
        self.mutate_public(id, expected_revision, |task| {
            task.resolve_conflict(policy, unix_time_ms())
        })
    }

    pub fn request_cancel(&self, id: TransferId) -> Result<(), ExecutorError> {
        self.queue
            .lock()
            .map_err(|_| ExecutorError::QueuePoisoned)?
            .mutate(id, |task| task.request_cancel(unix_time_ms()))?;
        if let Some(control) = self
            .active
            .lock()
            .map_err(|_| ExecutorError::QueuePoisoned)?
            .get(&id)
        {
            control.cancel();
        }
        Ok(())
    }

    pub fn request_pause(&self, _id: TransferId) -> Result<(), ExecutorError> {
        Err(ExecutorError::PauseUnsupported)
    }

    pub fn request_shutdown(&self) -> Result<usize, ExecutorError> {
        let active = self
            .active
            .lock()
            .map_err(|_| ExecutorError::QueuePoisoned)?
            .iter()
            .map(|(id, control)| (*id, control.clone()))
            .collect::<Vec<_>>();
        for (_, control) in &active {
            control.cancel();
        }
        self.permits.close();
        let now = unix_time_ms();
        let mut queue = self
            .queue
            .lock()
            .map_err(|_| ExecutorError::QueuePoisoned)?;
        for (id, _) in &active {
            queue.mutate(*id, |task| {
                if task.state.is_active() {
                    task.request_cancel(now)
                } else {
                    Ok(crate::StateChange::Unchanged)
                }
            })?;
        }
        Ok(active.len())
    }

    fn ensure_open(&self) -> Result<(), ExecutorError> {
        if self.permits.is_closed() {
            return Err(ExecutorError::ExecutorClosed);
        }
        Ok(())
    }

    fn mutate_public(
        &self,
        id: TransferId,
        expected_revision: u64,
        mutation: impl FnOnce(&mut TransferTask) -> Result<crate::StateChange, TransferMutationError>,
    ) -> Result<TransferMutationResult, ExecutorError> {
        let mut queue = self
            .queue
            .lock()
            .map_err(|_| ExecutorError::QueuePoisoned)?;
        let current = queue.task(id).cloned().ok_or(QueueError::TaskNotFound)?;
        current.validate_public().map_err(QueueError::from)?;
        if current.revision != expected_revision {
            return Ok(TransferMutationResult::Conflict {
                expected_revision,
                current,
            });
        }
        queue.mutate(id, mutation)?;
        let task = queue
            .task(id)
            .cloned()
            .ok_or(ExecutorError::TaskDisappeared)?;
        task.validate_public().map_err(QueueError::from)?;
        Ok(TransferMutationResult::Updated { task })
    }

    pub async fn run_next(&self) -> Result<Option<TransferRunOutcome>, ExecutorError> {
        let Some((task, token)) = self.start_next()? else {
            return Ok(None);
        };
        self.execute_started(task, token).await.map(Some)
    }

    pub async fn run_ready(&self) -> Result<Vec<TransferRunOutcome>, ExecutorError> {
        let mut jobs = JoinSet::new();
        let mut outcomes = Vec::new();
        loop {
            while jobs.len() < self.limits.max_concurrent {
                let Some((task, token)) = self.start_next()? else {
                    break;
                };
                let executor = self.clone();
                jobs.spawn(async move { executor.execute_started(task, token).await });
            }
            let Some(joined) = jobs.join_next().await else {
                break;
            };
            outcomes.push(joined.map_err(|_| ExecutorError::WorkerFailed)??);
        }
        Ok(outcomes)
    }

    fn start_next(&self) -> Result<Option<(crate::TransferTask, RunToken)>, ExecutorError> {
        let mut queue = self
            .queue
            .lock()
            .map_err(|_| ExecutorError::QueuePoisoned)?;
        let Some(token) = queue.start_next(unix_time_ms())? else {
            return Ok(None);
        };
        let task = queue
            .task(token.task_id)
            .cloned()
            .ok_or(ExecutorError::TaskDisappeared)?;
        Ok(Some((task, token)))
    }

    async fn execute_started(
        &self,
        task: crate::TransferTask,
        token: RunToken,
    ) -> Result<TransferRunOutcome, ExecutorError> {
        let _permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| ExecutorError::ExecutorClosed)?;
        let control = RemoteIoControl::new(Instant::now() + self.limits.total_timeout);
        self.active
            .lock()
            .map_err(|_| ExecutorError::QueuePoisoned)?
            .insert(task.id, control.clone());
        let cancelling = self
            .queue
            .lock()
            .map_err(|_| ExecutorError::QueuePoisoned)?
            .task(task.id)
            .is_some_and(|current| current.state.kind() == TransferStateKind::Cancelling);
        if cancelling {
            control.cancel();
        }
        let execution = self.execute_io(&task, token, control).await;
        let persisted = match execution {
            Ok(()) => Ok(()),
            Err(failure) => self.persist_failure(&task, token, failure),
        };
        self.active
            .lock()
            .map_err(|_| ExecutorError::QueuePoisoned)?
            .remove(&task.id);
        persisted?;
        let queue = self
            .queue
            .lock()
            .map_err(|_| ExecutorError::QueuePoisoned)?;
        let current = queue
            .task(task.id)
            .cloned()
            .ok_or(ExecutorError::TaskDisappeared)?;
        let state = current.state.kind();
        Ok(TransferRunOutcome {
            task_id: task.id,
            state,
        })
    }

    async fn execute_io(
        &self,
        task: &crate::TransferTask,
        token: RunToken,
        control: RemoteIoControl,
    ) -> Result<(), RemoteError> {
        let endpoint = remote_endpoint(task);
        ensure_transfer_protocol(endpoint.protocol)?;
        match self.remote.io_control_support(endpoint) {
            RemoteIoControlSupport::Supported => {}
            RemoteIoControlSupport::Unsupported(reason) => {
                return Err(RemoteError::new(
                    RemoteErrorKind::Unsupported,
                    RemoteOperation::Connect,
                    reason,
                    RetryDisposition::Never,
                ));
            }
        }
        let session = self
            .remote
            .open(endpoint, self.operation_control(&control))
            .await?;
        let snapshot = session.snapshot();
        if snapshot.profile_id != endpoint.profile_id || snapshot.protocol != endpoint.protocol {
            let _ = session.disconnect_controlled(self.cleanup_control()).await;
            return Err(protocol_error(
                RemoteOperation::Connect,
                "transfer_session_identity_mismatch",
            ));
        }
        match session.io_control_support() {
            RemoteIoControlSupport::Supported => {}
            RemoteIoControlSupport::Unsupported(reason) => {
                let _ = session.disconnect_controlled(self.cleanup_control()).await;
                return Err(RemoteError::new(
                    RemoteErrorKind::Unsupported,
                    RemoteOperation::Connect,
                    reason,
                    RetryDisposition::Never,
                ));
            }
        }
        let result = async {
            let required = match task.direction {
                TransferDirection::Upload => FileOperation::Write,
                TransferDirection::Download => FileOperation::Read,
            };
            require_capability(&snapshot.capabilities, required)?;
            if task.progress.bytes_transferred > 0 {
                require_capability(&snapshot.capabilities, task.direction.resume_operation())?;
            }
            match task.direction {
                TransferDirection::Upload => {
                    self.upload(task, token, session.as_ref(), control.clone())
                        .await
                }
                TransferDirection::Download => {
                    self.download(task, token, session.as_ref(), control.clone())
                        .await
                }
            }
        }
        .await;
        let disconnect = session.disconnect_controlled(self.cleanup_control()).await;
        match (result, disconnect) {
            (Err(value), _) => Err(value),
            (Ok(()), Err(value)) => Err(value),
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    async fn upload(
        &self,
        task: &crate::TransferTask,
        token: RunToken,
        session: &dyn RemoteFileSession,
        control: RemoteIoControl,
    ) -> Result<(), RemoteError> {
        let local = local_handle(task);
        let remote = remote_endpoint(task);
        let source_identity = self
            .local
            .source_identity(local, self.operation_control(&control))
            .await?;
        ensure_identity(
            task.expected_source.as_ref(),
            Some(&source_identity),
            RemoteOperation::Read,
        )?;
        let total = source_identity.size_bytes;
        let offset = task.progress.bytes_transferred;
        let final_path = upload_final_path(task, &remote.path)?;
        let temporary_path = RemotePath::new(format!(
            "{}.localdesk-{}.part",
            final_path.as_str(),
            task.id.as_uuid()
        ))
        .map_err(|_| invalid(RemoteOperation::Write, "transfer_staged_path_invalid"))?;
        let receipt = session
            .begin_write_controlled(
                BeginWriteRequest {
                    final_path,
                    temporary_path,
                    expected_size_bytes: total,
                    resume_from: (offset > 0).then_some(offset),
                    expected_destination: task.expected_destination.clone(),
                },
                self.operation_control(&control),
            )
            .await?;
        if receipt.next_offset != offset {
            return Err(protocol_error(
                RemoteOperation::Write,
                "remote_write_offset_mismatch",
            ));
        }
        let write_handle = receipt.handle;
        let result = self
            .upload_started(
                task,
                token,
                session,
                &control,
                local,
                source_identity,
                total,
                offset,
                write_handle,
            )
            .await;
        if result.is_err() {
            let _ = session
                .abort_write_controlled(write_handle, self.cleanup_control())
                .await;
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn upload_started(
        &self,
        task: &crate::TransferTask,
        token: RunToken,
        session: &dyn RemoteFileSession,
        control: &RemoteIoControl,
        local: LocalFileHandle,
        source_identity: ObjectIdentity,
        total: Option<u64>,
        mut offset: u64,
        write_handle: localdesk_remote_core::RemoteWriteHandle,
    ) -> Result<(), RemoteError> {
        let started = Instant::now();
        while !total.is_some_and(|size| offset >= size) {
            control.check(RemoteOperation::Read)?;
            let chunk = self
                .local
                .read_chunk(
                    local,
                    offset,
                    self.limits.chunk_bytes,
                    Some(source_identity.clone()),
                    self.operation_control(control),
                )
                .await?;
            if chunk.offset != offset || (chunk.bytes.is_empty() && !chunk.eof) {
                return Err(protocol_error(
                    RemoteOperation::Read,
                    "local_read_chunk_invalid",
                ));
            }
            if !chunk.bytes.is_empty() {
                let count = chunk.bytes.len() as u64;
                let receipt = session
                    .write_chunk_controlled(
                        write_handle,
                        offset,
                        chunk.bytes,
                        self.operation_control(control),
                    )
                    .await?;
                offset = offset.saturating_add(count);
                if receipt.next_offset != offset {
                    return Err(protocol_error(
                        RemoteOperation::Write,
                        "remote_write_offset_mismatch",
                    ));
                }
                self.throttle_and_record(task, token, offset, total, started, control)
                    .await?;
            }
            if chunk.eof {
                break;
            }
        }
        ensure_final_size(offset, total, RemoteOperation::Read)?;
        self.record_zero_progress(task, token, offset, total)?;
        let entry = session
            .commit_write_controlled(
                write_handle,
                task.expected_destination.clone(),
                self.operation_control(control),
            )
            .await?;
        self.complete(task, token, entry.identity)
    }

    async fn download(
        &self,
        task: &crate::TransferTask,
        token: RunToken,
        session: &dyn RemoteFileSession,
        control: RemoteIoControl,
    ) -> Result<(), RemoteError> {
        let local = local_handle(task);
        let remote = remote_endpoint(task);
        let offset = task.progress.bytes_transferred;
        let total = task
            .expected_source
            .as_ref()
            .and_then(|value| value.size_bytes);
        let receipt = self
            .local
            .begin_staged_write(
                local,
                total,
                (offset > 0).then_some(offset),
                task.expected_destination.clone(),
                task.conflict_policy,
                self.operation_control(&control),
            )
            .await?;
        if receipt.next_offset != offset {
            return Err(protocol_error(
                RemoteOperation::Write,
                "local_write_offset_mismatch",
            ));
        }
        let write_handle = receipt.handle;
        let result = self
            .download_started(
                task,
                token,
                session,
                &control,
                remote.path.clone(),
                total,
                offset,
                write_handle,
            )
            .await;
        if result.is_err() {
            let _ = self
                .local
                .abort_staged_write(write_handle, self.cleanup_control())
                .await;
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn download_started(
        &self,
        task: &crate::TransferTask,
        token: RunToken,
        session: &dyn RemoteFileSession,
        control: &RemoteIoControl,
        remote_path: RemotePath,
        mut total: Option<u64>,
        mut offset: u64,
        write_handle: LocalWriteHandle,
    ) -> Result<(), RemoteError> {
        let started = Instant::now();
        loop {
            control.check(RemoteOperation::Read)?;
            let chunk = session
                .read_chunk_controlled(
                    RemoteReadRequest {
                        path: remote_path.clone(),
                        offset,
                        max_bytes: self.limits.chunk_bytes,
                        expected_identity: task.expected_source.clone(),
                    },
                    self.operation_control(control),
                )
                .await?;
            if chunk.offset != offset || (chunk.bytes.is_empty() && !chunk.eof) {
                return Err(protocol_error(
                    RemoteOperation::Read,
                    "remote_read_chunk_invalid",
                ));
            }
            total = total.or(chunk.identity.size_bytes);
            if !chunk.bytes.is_empty() {
                let count = chunk.bytes.len() as u64;
                let receipt = self
                    .local
                    .write_chunk(
                        write_handle,
                        offset,
                        chunk.bytes,
                        self.operation_control(control),
                    )
                    .await?;
                offset = offset.saturating_add(count);
                if receipt.next_offset != offset {
                    return Err(protocol_error(
                        RemoteOperation::Write,
                        "local_write_offset_mismatch",
                    ));
                }
                self.throttle_and_record(task, token, offset, total, started, control)
                    .await?;
            }
            if chunk.eof {
                break;
            }
        }
        ensure_final_size(offset, total, RemoteOperation::Read)?;
        self.record_zero_progress(task, token, offset, total)?;
        let identity = self
            .local
            .commit_staged_write(write_handle, total, self.operation_control(control))
            .await?;
        self.complete(task, token, identity)
    }

    fn operation_control(&self, total: &RemoteIoControl) -> RemoteIoControl {
        total.with_deadline(Instant::now() + self.limits.operation_timeout)
    }

    fn cleanup_control(&self) -> RemoteIoControl {
        RemoteIoControl::new(Instant::now() + self.limits.operation_timeout)
    }

    async fn throttle_and_record(
        &self,
        task: &crate::TransferTask,
        token: RunToken,
        bytes: u64,
        total: Option<u64>,
        started: Instant,
        control: &RemoteIoControl,
    ) -> Result<(), RemoteError> {
        if let Some(limit) = task.bandwidth_limit.get() {
            let expected = Duration::from_secs_f64(bytes as f64 / limit.get() as f64);
            while expected > started.elapsed() {
                control.check(RemoteOperation::Write)?;
                let remaining = expected - started.elapsed();
                tokio::time::sleep(remaining.min(Duration::from_millis(100))).await;
            }
        }
        control.check(RemoteOperation::Write)?;
        let elapsed_ms = started.elapsed().as_millis().max(1) as u64;
        let speed = bytes.saturating_mul(1_000).checked_div(elapsed_ms);
        self.record_progress(task.id, token, bytes, total, speed)
    }

    fn record_progress(
        &self,
        id: TransferId,
        token: RunToken,
        bytes: u64,
        total: Option<u64>,
        speed: Option<u64>,
    ) -> Result<(), RemoteError> {
        self.queue
            .lock()
            .map_err(|_| queue_error())?
            .mutate(id, |current| {
                current.record_progress(token, bytes, total, speed, unix_time_ms())
            })
            .map_err(map_queue_error)?;
        Ok(())
    }

    fn record_zero_progress(
        &self,
        task: &crate::TransferTask,
        token: RunToken,
        offset: u64,
        total: Option<u64>,
    ) -> Result<(), RemoteError> {
        if task.progress.bytes_transferred == offset && total == Some(offset) {
            self.record_progress(task.id, token, offset, total, None)?;
        }
        Ok(())
    }

    fn complete(
        &self,
        task: &crate::TransferTask,
        token: RunToken,
        identity: ObjectIdentity,
    ) -> Result<(), RemoteError> {
        let verification = match remote_endpoint(task).protocol {
            RemoteProtocol::Sftp | RemoteProtocol::Smb => VerificationLevel::RemoteIdentity,
            RemoteProtocol::Ftp | RemoteProtocol::FtpsExplicit => VerificationLevel::Size,
            RemoteProtocol::Ssh => VerificationLevel::Unverified,
        };
        self.queue
            .lock()
            .map_err(|_| queue_error())?
            .mutate(task.id, |current| {
                current.complete(
                    token,
                    TransferCompletion {
                        verification,
                        identity: Some(identity),
                        completed_at_unix_ms: unix_time_ms(),
                    },
                )
            })
            .map_err(map_queue_error)?;
        Ok(())
    }

    fn persist_failure(
        &self,
        task: &crate::TransferTask,
        token: RunToken,
        failure: RemoteError,
    ) -> Result<(), ExecutorError> {
        let now = unix_time_ms();
        let mut queue = self
            .queue
            .lock()
            .map_err(|_| ExecutorError::QueuePoisoned)?;
        let current = queue
            .task(task.id)
            .cloned()
            .ok_or(ExecutorError::TaskDisappeared)?;
        let state = current.state.kind();
        let checkpoint = checkpoint(&current, now);
        if state == TransferStateKind::Cancelling || failure.kind == RemoteErrorKind::Cancelled {
            queue.mutate(task.id, |current| {
                current.confirm_cancelled(token, Some(checkpoint.clone()), now)
            })?;
            return Ok(());
        }
        if failure.kind == RemoteErrorKind::Conflict {
            queue.mutate(task.id, |current| {
                current.enter_conflict(
                    token,
                    crate::TransferConflict {
                        reason: failure.reason,
                        checkpoint: Some(checkpoint.clone()),
                    },
                    now,
                )
            })?;
            return Ok(());
        }
        let retryable = failure.is_retryable();
        queue.mutate(task.id, |current| {
            current.fail(token, TransferFailure::from(failure), now)
        })?;
        if retryable {
            let result = queue.mutate(task.id, |current| current.schedule_retry(now));
            if !matches!(
                result,
                Err(QueueError::Mutation(TransferMutationError::RetryExhausted))
            ) {
                result?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ExecutorError {
    #[error("transfer queue is poisoned")]
    QueuePoisoned,
    #[error("transfer task disappeared from the sole-writer queue")]
    TaskDisappeared,
    #[error("transfer executor is closed")]
    ExecutorClosed,
    #[error("transfer worker failed")]
    WorkerFailed,
    #[error("in-flight pause is not supported by the current executor")]
    PauseUnsupported,
    #[error(transparent)]
    Queue(#[from] QueueError),
    #[error(transparent)]
    Remote(#[from] RemoteError),
}

fn remote_endpoint(task: &crate::TransferTask) -> &crate::RemoteTransferEndpoint {
    task.source
        .remote()
        .or_else(|| task.destination.remote())
        .expect("validated transfer has one remote endpoint")
}

fn local_handle(task: &crate::TransferTask) -> LocalFileHandle {
    match (&task.source, &task.destination) {
        (TransferEndpoint::Local { handle }, _) | (_, TransferEndpoint::Local { handle }) => {
            *handle
        }
        _ => unreachable!("validated transfer has one local endpoint"),
    }
}

fn ensure_transfer_protocol(protocol: RemoteProtocol) -> Result<(), RemoteError> {
    match protocol {
        RemoteProtocol::Sftp
        | RemoteProtocol::Ftp
        | RemoteProtocol::FtpsExplicit
        | RemoteProtocol::Smb => Ok(()),
        RemoteProtocol::Ssh => Err(unsupported(RemoteOperation::Connect, "ssh_terminal_only")),
    }
}

fn require_capability(
    capabilities: &localdesk_remote_core::CapabilityMatrix,
    operation: FileOperation,
) -> Result<(), RemoteError> {
    match capabilities.status(operation) {
        CapabilityStatus::Supported => Ok(()),
        CapabilityStatus::Unsupported(reason) => Err(RemoteError::new(
            RemoteErrorKind::Unsupported,
            match operation {
                FileOperation::Read => RemoteOperation::Read,
                FileOperation::Write => RemoteOperation::Write,
                FileOperation::ResumeRead | FileOperation::ResumeWrite => RemoteOperation::Resume,
                _ => RemoteOperation::Connect,
            },
            reason.clone(),
            RetryDisposition::Never,
        )),
    }
}

fn upload_final_path(
    task: &crate::TransferTask,
    path: &RemotePath,
) -> Result<RemotePath, RemoteError> {
    if task.conflict_policy == ConflictPolicy::Rename {
        RemotePath::new(format!("{}.localdesk-{}", path.as_str(), task.id.as_uuid()))
            .map_err(|_| invalid(RemoteOperation::Write, "transfer_renamed_path_invalid"))
    } else {
        Ok(path.clone())
    }
}

fn checkpoint(task: &crate::TransferTask, now: i64) -> TransferCheckpoint {
    let verification = match task.features.resume_validation {
        Some(ResumeValidation::RemoteIdentity) => VerificationLevel::RemoteIdentity,
        Some(ResumeValidation::SizeOnly) => VerificationLevel::Size,
        None => VerificationLevel::Unverified,
    };
    TransferCheckpoint {
        offset: task.progress.bytes_transferred,
        source_identity: task.expected_source.clone(),
        destination_identity: task.expected_destination.clone(),
        verification,
        verified_at_unix_ms: now,
    }
}

fn ensure_final_size(
    transferred: u64,
    expected: Option<u64>,
    operation: RemoteOperation,
) -> Result<(), RemoteError> {
    if expected.is_some_and(|value| value != transferred) {
        Err(error(
            RemoteErrorKind::Conflict,
            operation,
            "transfer_source_size_changed",
            RetryDisposition::UserAction,
        ))
    } else {
        Ok(())
    }
}

fn validate_owned_file(
    path: &Path,
    uid: u32,
    operation: RemoteOperation,
) -> Result<ObjectIdentity, RemoteError> {
    if !path.is_absolute() {
        return Err(invalid(operation, "local_owner_path_must_be_absolute"));
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| {
        error(
            RemoteErrorKind::NotFound,
            operation,
            "local_file_not_found",
            RetryDisposition::UserAction,
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.uid() != uid {
        return Err(error(
            RemoteErrorKind::PermissionDenied,
            operation,
            "local_file_unsafe",
            RetryDisposition::Never,
        ));
    }
    Ok(identity(&metadata))
}

fn open_source_file(
    path: &Path,
    operation: RemoteOperation,
) -> Result<(fs::File, ObjectIdentity), RemoteError> {
    if !path.is_absolute() {
        return Err(invalid(operation, "local_owner_path_must_be_absolute"));
    }
    let path_metadata = fs::symlink_metadata(path).map_err(|_| {
        error(
            RemoteErrorKind::NotFound,
            operation,
            "local_file_not_found",
            RetryDisposition::UserAction,
        )
    })?;
    if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
        return Err(error(
            RemoteErrorKind::PermissionDenied,
            operation,
            "local_file_unsafe",
            RetryDisposition::Never,
        ));
    }
    let file = open_file(path, false, false, false, operation)?;
    let metadata = file
        .metadata()
        .map_err(|_| io_error(operation, "local_source_metadata_failed"))?;
    if !metadata.is_file() {
        return Err(error(
            RemoteErrorKind::PermissionDenied,
            operation,
            "local_file_unsafe",
            RetryDisposition::Never,
        ));
    }
    Ok((file, identity(&metadata)))
}

fn validate_destination(path: &Path, uid: u32) -> Result<(), RemoteError> {
    if !path.is_absolute() {
        return Err(invalid(
            RemoteOperation::Write,
            "local_owner_path_must_be_absolute",
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| invalid(RemoteOperation::Write, "local_destination_parent_invalid"))?;
    let metadata = fs::symlink_metadata(parent).map_err(|_| stat_error())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() || metadata.uid() != uid {
        return Err(error(
            RemoteErrorKind::PermissionDenied,
            RemoteOperation::Write,
            "local_destination_parent_unsafe",
            RetryDisposition::Never,
        ));
    }
    if path.exists() {
        validate_owned_file(path, uid, RemoteOperation::Write)?;
    }
    Ok(())
}

fn optional_identity(
    path: &Path,
    uid: u32,
    operation: RemoteOperation,
) -> Result<Option<ObjectIdentity>, RemoteError> {
    match fs::symlink_metadata(path) {
        Ok(_) => validate_owned_file(path, uid, operation).map(Some),
        Err(value) if value.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(stat_error()),
    }
}

fn identity(metadata: &fs::Metadata) -> ObjectIdentity {
    ObjectIdentity {
        size_bytes: Some(metadata.len()),
        modified_at_unix_ms: metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .and_then(|value| i64::try_from(value.as_millis()).ok()),
        etag: None,
    }
}

fn ensure_identity(
    expected: Option<&ObjectIdentity>,
    actual: Option<&ObjectIdentity>,
    operation: RemoteOperation,
) -> Result<(), RemoteError> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let matches = actual.is_some_and(|actual| {
        expected
            .size_bytes
            .is_none_or(|value| actual.size_bytes == Some(value))
            && expected
                .modified_at_unix_ms
                .is_none_or(|value| actual.modified_at_unix_ms == Some(value))
            && expected
                .etag
                .as_ref()
                .is_none_or(|value| actual.etag.as_ref() == Some(value))
    });
    if matches {
        Ok(())
    } else {
        Err(error(
            RemoteErrorKind::Conflict,
            operation,
            "transfer_object_identity_changed",
            RetryDisposition::UserAction,
        ))
    }
}

fn open_file(
    path: &Path,
    write: bool,
    create: bool,
    truncate: bool,
    operation: RemoteOperation,
) -> Result<fs::File, RemoteError> {
    let mut options = OpenOptions::new();
    options
        .read(!write)
        .write(write)
        .create(create)
        .truncate(truncate)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .mode(0o600);
    options.open(path).map_err(|open_error| {
        if !write && open_error.kind() == std::io::ErrorKind::PermissionDenied {
            error(
                RemoteErrorKind::PermissionDenied,
                operation,
                "local_file_permission_denied",
                RetryDisposition::UserAction,
            )
        } else {
            io_error(
                operation,
                if write {
                    "local_staged_open_failed"
                } else {
                    "local_source_open_failed"
                },
            )
        }
    })
}

fn staged_path(path: &Path, handle: LocalFileHandle) -> Result<PathBuf, RemoteError> {
    Ok(path
        .parent()
        .ok_or_else(stat_error)?
        .join(format!(".localdesk-transfer-{}.part", handle.as_uuid())))
}

fn renamed_path(path: &Path, handle: LocalFileHandle) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("download");
    path.with_file_name(format!("{name}.localdesk-{}", handle.as_uuid()))
}

fn unix_time_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|value| i64::try_from(value.as_millis()).ok())
        .unwrap_or(i64::MAX)
}

fn error(
    kind: RemoteErrorKind,
    operation: RemoteOperation,
    reason: &'static str,
    retry: RetryDisposition,
) -> RemoteError {
    RemoteError::new(
        kind,
        operation,
        SafeReason::new(reason).expect("static reason is valid"),
        retry,
    )
}

fn invalid(operation: RemoteOperation, reason: &'static str) -> RemoteError {
    error(
        RemoteErrorKind::InvalidInput,
        operation,
        reason,
        RetryDisposition::Never,
    )
}

fn unsupported(operation: RemoteOperation, reason: &'static str) -> RemoteError {
    error(
        RemoteErrorKind::Unsupported,
        operation,
        reason,
        RetryDisposition::Never,
    )
}

fn protocol_error(operation: RemoteOperation, reason: &'static str) -> RemoteError {
    error(
        RemoteErrorKind::RemoteProtocol,
        operation,
        reason,
        RetryDisposition::Never,
    )
}

fn io_error(operation: RemoteOperation, reason: &'static str) -> RemoteError {
    error(
        RemoteErrorKind::Transport,
        operation,
        reason,
        RetryDisposition::Backoff,
    )
}

fn owner_poisoned(operation: RemoteOperation) -> RemoteError {
    io_error(operation, "local_handle_owner_poisoned")
}

fn stat_error() -> RemoteError {
    io_error(RemoteOperation::Stat, "local_file_stat_failed")
}

fn missing_write_handle() -> RemoteError {
    error(
        RemoteErrorKind::NotFound,
        RemoteOperation::Write,
        "local_write_handle_not_found",
        RetryDisposition::Never,
    )
}

fn queue_error() -> RemoteError {
    io_error(RemoteOperation::Write, "transfer_queue_unavailable")
}

fn map_queue_error(value: QueueError) -> RemoteError {
    match value {
        QueueError::Mutation(TransferMutationError::StaleRunToken) => error(
            RemoteErrorKind::Cancelled,
            RemoteOperation::Write,
            "transfer_run_token_stale",
            RetryDisposition::Never,
        ),
        _ => queue_error(),
    }
}
