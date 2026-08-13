use localdesk_remote_core::{
    AdapterFuture, BeginWriteRequest, CapabilityMatrix, CapabilityStatus, ConnectionState,
    EntryKind, FILE_OPERATIONS, FileOperation, ObjectIdentity, OperationCapability, ProfileId,
    RemoteEntry, RemoteError, RemoteErrorKind, RemoteFileSession, RemoteIoControl,
    RemoteIoControlSupport, RemoteOperation, RemotePath, RemoteProtocol, RemoteReadChunk,
    RemoteReadRequest, RemoteSession, RemoteWriteHandle, RemoteWriteReceipt, RetryDisposition,
    SafeReason, SessionId,
};
use localdesk_transfers::{
    BandwidthLimit, ConflictPolicy, ExecutorLimits, FeatureSupport, InMemoryTransferStore,
    LocalFileHandle, LocalHandleOwner, QueueLimits, RemoteSessionFactory, RemoteTransferEndpoint,
    RetryPolicy, TransferDirection, TransferEndpoint, TransferExecutor, TransferFeatureSet,
    TransferId, TransferQueue, TransferState, TransferStateKind, TransferTask,
};
use std::{
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    sync::{Arc, Mutex},
    time::Duration,
};
use tempfile::tempdir;
use tokio::sync::Notify;

fn reason(value: &'static str) -> SafeReason {
    SafeReason::new(value).expect("static reason")
}

fn remote_error(
    kind: RemoteErrorKind,
    operation: RemoteOperation,
    value: &'static str,
) -> RemoteError {
    RemoteError::new(kind, operation, reason(value), RetryDisposition::Never)
}

fn capabilities(read: bool, write: bool) -> CapabilityMatrix {
    CapabilityMatrix::complete(FILE_OPERATIONS.iter().copied().map(|operation| {
        let supported = matches!(operation, FileOperation::Read) && read
            || matches!(operation, FileOperation::Write) && write;
        OperationCapability {
            operation,
            status: if supported {
                CapabilityStatus::Supported
            } else {
                CapabilityStatus::Unsupported(reason("fixture_operation_unsupported"))
            },
        }
    }))
    .expect("complete capabilities")
}

#[derive(Default)]
struct FakeState {
    download: Vec<u8>,
    upload: Vec<u8>,
    write_handle: Option<RemoteWriteHandle>,
    opens: u32,
    aborts: u32,
    disconnects: u32,
    block_reads: bool,
    fail_write_after: Option<usize>,
}

struct FakeFactory {
    profile_id: ProfileId,
    state: Arc<Mutex<FakeState>>,
    opened: Arc<Notify>,
    capabilities: CapabilityMatrix,
}

impl RemoteSessionFactory for FakeFactory {
    fn io_control_support(&self, _endpoint: &RemoteTransferEndpoint) -> RemoteIoControlSupport {
        RemoteIoControlSupport::Supported
    }

    fn open<'a>(
        &'a self,
        endpoint: &'a RemoteTransferEndpoint,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<Box<dyn RemoteFileSession>, RemoteError>> {
        Box::pin(async move {
            control.check(RemoteOperation::Connect)?;
            self.state.lock().expect("state").opens += 1;
            self.opened.notify_one();
            Ok(Box::new(FakeSession {
                id: SessionId::new(),
                profile_id: self.profile_id,
                protocol: endpoint.protocol,
                state: Arc::clone(&self.state),
                capabilities: self.capabilities.clone(),
            }) as Box<dyn RemoteFileSession>)
        })
    }
}

struct FakeSession {
    id: SessionId,
    profile_id: ProfileId,
    protocol: RemoteProtocol,
    state: Arc<Mutex<FakeState>>,
    capabilities: CapabilityMatrix,
}

impl FakeSession {
    fn unsupported<'a, T: Send + 'a>(
        operation: RemoteOperation,
    ) -> AdapterFuture<'a, Result<T, RemoteError>> {
        Box::pin(async move {
            Err(remote_error(
                RemoteErrorKind::Unsupported,
                operation,
                "fixture_operation_unsupported",
            ))
        })
    }

    fn identity(size: usize) -> ObjectIdentity {
        ObjectIdentity {
            size_bytes: Some(size as u64),
            modified_at_unix_ms: Some(1),
            etag: Some("fixture-v1".to_owned()),
        }
    }
}

impl RemoteFileSession for FakeSession {
    fn id(&self) -> SessionId {
        self.id
    }

    fn snapshot(&self) -> RemoteSession {
        RemoteSession {
            id: self.id,
            profile_id: self.profile_id,
            protocol: self.protocol,
            state: ConnectionState::Ready,
            capabilities: self.capabilities.clone(),
            opened_at_unix_ms: 1,
            updated_at_unix_ms: 1,
        }
    }

    fn io_control_support(&self) -> RemoteIoControlSupport {
        RemoteIoControlSupport::Supported
    }

    fn list<'a>(
        &'a self,
        _path: &'a RemotePath,
    ) -> AdapterFuture<'a, Result<Vec<RemoteEntry>, RemoteError>> {
        Self::unsupported(RemoteOperation::List)
    }

    fn stat<'a>(
        &'a self,
        _path: &'a RemotePath,
    ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>> {
        Self::unsupported(RemoteOperation::Stat)
    }

    fn create_directory<'a>(
        &'a self,
        _path: &'a RemotePath,
    ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>> {
        Self::unsupported(RemoteOperation::CreateDirectory)
    }

    fn rename<'a>(
        &'a self,
        _from: &'a RemotePath,
        _to: &'a RemotePath,
    ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>> {
        Self::unsupported(RemoteOperation::Rename)
    }

    fn delete<'a>(&'a self, _path: &'a RemotePath) -> AdapterFuture<'a, Result<(), RemoteError>> {
        Self::unsupported(RemoteOperation::Delete)
    }

    fn read_chunk<'a>(
        &'a self,
        _request: RemoteReadRequest,
    ) -> AdapterFuture<'a, Result<RemoteReadChunk, RemoteError>> {
        Self::unsupported(RemoteOperation::Read)
    }

    fn begin_write<'a>(
        &'a self,
        _request: BeginWriteRequest,
    ) -> AdapterFuture<'a, Result<RemoteWriteReceipt, RemoteError>> {
        Self::unsupported(RemoteOperation::Write)
    }

    fn write_chunk<'a>(
        &'a self,
        _handle: RemoteWriteHandle,
        _offset: u64,
        _bytes: Vec<u8>,
    ) -> AdapterFuture<'a, Result<RemoteWriteReceipt, RemoteError>> {
        Self::unsupported(RemoteOperation::Write)
    }

    fn commit_write<'a>(
        &'a self,
        _handle: RemoteWriteHandle,
        _expected_identity: Option<ObjectIdentity>,
    ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>> {
        Self::unsupported(RemoteOperation::Write)
    }

    fn abort_write<'a>(
        &'a self,
        _handle: RemoteWriteHandle,
    ) -> AdapterFuture<'a, Result<(), RemoteError>> {
        Self::unsupported(RemoteOperation::Write)
    }

    fn read_chunk_controlled<'a>(
        &'a self,
        request: RemoteReadRequest,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<RemoteReadChunk, RemoteError>> {
        Box::pin(async move {
            loop {
                control.check(RemoteOperation::Read)?;
                if !self.state.lock().expect("state").block_reads {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let state = self.state.lock().expect("state");
            let start = usize::try_from(request.offset)
                .unwrap_or(usize::MAX)
                .min(state.download.len());
            let end = start
                .saturating_add(request.max_bytes as usize)
                .min(state.download.len());
            Ok(RemoteReadChunk {
                offset: request.offset,
                bytes: state.download[start..end].to_vec(),
                eof: end == state.download.len(),
                identity: Self::identity(state.download.len()),
            })
        })
    }

    fn begin_write_controlled<'a>(
        &'a self,
        request: BeginWriteRequest,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<RemoteWriteReceipt, RemoteError>> {
        Box::pin(async move {
            control.check(RemoteOperation::Write)?;
            let mut state = self.state.lock().expect("state");
            let offset = request.resume_from.unwrap_or(0);
            if offset == 0 {
                state.upload.clear();
            } else if state.upload.len() as u64 != offset {
                return Err(remote_error(
                    RemoteErrorKind::Conflict,
                    RemoteOperation::Resume,
                    "fixture_resume_offset_mismatch",
                ));
            }
            let handle = RemoteWriteHandle::new();
            state.write_handle = Some(handle);
            Ok(RemoteWriteReceipt {
                handle,
                next_offset: offset,
                identity: None,
            })
        })
    }

    fn write_chunk_controlled<'a>(
        &'a self,
        handle: RemoteWriteHandle,
        offset: u64,
        bytes: Vec<u8>,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<RemoteWriteReceipt, RemoteError>> {
        Box::pin(async move {
            control.check(RemoteOperation::Write)?;
            let mut state = self.state.lock().expect("state");
            if state.write_handle != Some(handle) || state.upload.len() as u64 != offset {
                return Err(remote_error(
                    RemoteErrorKind::Conflict,
                    RemoteOperation::Write,
                    "fixture_write_offset_mismatch",
                ));
            }
            if state
                .fail_write_after
                .is_some_and(|limit| state.upload.len() >= limit)
            {
                return Err(remote_error(
                    RemoteErrorKind::Transport,
                    RemoteOperation::Write,
                    "fixture_write_failed",
                ));
            }
            state.upload.extend_from_slice(&bytes);
            Ok(RemoteWriteReceipt {
                handle,
                next_offset: state.upload.len() as u64,
                identity: None,
            })
        })
    }

    fn commit_write_controlled<'a>(
        &'a self,
        handle: RemoteWriteHandle,
        _expected_identity: Option<ObjectIdentity>,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>> {
        Box::pin(async move {
            control.check(RemoteOperation::Write)?;
            let mut state = self.state.lock().expect("state");
            if state.write_handle.take() != Some(handle) {
                return Err(remote_error(
                    RemoteErrorKind::Conflict,
                    RemoteOperation::Write,
                    "fixture_write_handle_mismatch",
                ));
            }
            Ok(RemoteEntry {
                name: "fixture.bin".to_owned(),
                path: RemotePath::new("/fixture.bin").expect("path"),
                kind: EntryKind::File,
                identity: Self::identity(state.upload.len()),
                unix_mode: None,
                capabilities: self.capabilities.clone(),
            })
        })
    }

    fn abort_write_controlled<'a>(
        &'a self,
        handle: RemoteWriteHandle,
        _control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<(), RemoteError>> {
        Box::pin(async move {
            let mut state = self.state.lock().expect("state");
            if state.write_handle.take() == Some(handle) {
                state.aborts += 1;
            }
            Ok(())
        })
    }

    fn disconnect<'a>(&'a self) -> AdapterFuture<'a, Result<(), RemoteError>> {
        Box::pin(async { Ok(()) })
    }

    fn disconnect_controlled<'a>(
        &'a self,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<(), RemoteError>> {
        Box::pin(async move {
            control.check(RemoteOperation::Disconnect)?;
            self.state.lock().expect("state").disconnects += 1;
            Ok(())
        })
    }
}

struct UnsupportedFactory;

impl RemoteSessionFactory for UnsupportedFactory {
    fn io_control_support(&self, _endpoint: &RemoteTransferEndpoint) -> RemoteIoControlSupport {
        RemoteIoControlSupport::Unsupported(reason("fixture_io_control_unsupported"))
    }

    fn open<'a>(
        &'a self,
        _endpoint: &'a RemoteTransferEndpoint,
        _control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<Box<dyn RemoteFileSession>, RemoteError>> {
        Box::pin(async {
            panic!("unsupported factories must be rejected before opening a session")
        })
    }
}

fn task(
    direction: TransferDirection,
    profile_id: ProfileId,
    local: LocalFileHandle,
    content_size: usize,
    capabilities: &CapabilityMatrix,
) -> TransferTask {
    let remote = TransferEndpoint::Remote(RemoteTransferEndpoint {
        profile_id,
        protocol: RemoteProtocol::Sftp,
        path: RemotePath::new("/fixture.bin").expect("path"),
    });
    let local = TransferEndpoint::Local { handle: local };
    let (source, destination, expected_source) = match direction {
        TransferDirection::Upload => (local, remote, None),
        TransferDirection::Download => (remote, local, Some(FakeSession::identity(content_size))),
    };
    TransferTask::new(
        TransferId::new(),
        source,
        destination,
        direction,
        expected_source,
        None,
        RetryPolicy::default(),
        BandwidthLimit::unlimited(),
        ConflictPolicy::Fail,
        TransferFeatureSet::from_adapter(direction, RemoteProtocol::Sftp, capabilities),
        1,
    )
    .expect("task")
}

fn executor(
    task: TransferTask,
    owner: Arc<LocalHandleOwner>,
    factory: Arc<FakeFactory>,
) -> TransferExecutor<InMemoryTransferStore> {
    let mut queue =
        TransferQueue::open(InMemoryTransferStore::default(), QueueLimits::default(), 1)
            .expect("queue");
    queue.enqueue(task).expect("enqueue");
    TransferExecutor::new(
        queue,
        owner,
        factory,
        ExecutorLimits::new(1, 2, Duration::from_secs(1), Duration::from_secs(5)).expect("limits"),
    )
}

#[tokio::test]
async fn download_streams_real_chunks_into_an_atomic_local_commit() {
    let directory = tempdir().expect("directory");
    let destination = directory.path().join("download.bin");
    let local = LocalFileHandle::new();
    let owner = Arc::new(LocalHandleOwner::default());
    owner.bind_destination(local, &destination).expect("bind");
    let profile_id = ProfileId::new();
    let caps = capabilities(true, false);
    let state = Arc::new(Mutex::new(FakeState {
        download: b"abcdef".to_vec(),
        ..FakeState::default()
    }));
    let factory = Arc::new(FakeFactory {
        profile_id,
        state,
        opened: Arc::new(Notify::new()),
        capabilities: caps.clone(),
    });
    let task = task(TransferDirection::Download, profile_id, local, 6, &caps);
    let id = task.id;
    let executor = executor(task, owner, factory);

    let outcome = executor.run_next().await.expect("run").expect("task");
    assert_eq!(outcome.state, TransferStateKind::Completed);
    assert_eq!(std::fs::read(destination).expect("download"), b"abcdef");
    assert_eq!(
        executor
            .queue()
            .lock()
            .expect("queue")
            .task(id)
            .expect("task")
            .progress
            .bytes_transferred,
        6
    );
}

#[tokio::test]
async fn upload_streams_real_local_chunks_into_remote_staged_commit() {
    let directory = tempdir().expect("directory");
    let source = directory.path().join("upload.bin");
    std::fs::write(&source, b"upload-data").expect("source");
    let local = LocalFileHandle::new();
    let owner = Arc::new(LocalHandleOwner::default());
    owner.bind_source(local, &source).expect("bind");
    let profile_id = ProfileId::new();
    let caps = capabilities(false, true);
    let state = Arc::new(Mutex::new(FakeState::default()));
    let factory = Arc::new(FakeFactory {
        profile_id,
        state: Arc::clone(&state),
        opened: Arc::new(Notify::new()),
        capabilities: caps.clone(),
    });
    let task = task(TransferDirection::Upload, profile_id, local, 11, &caps);
    let executor = executor(task, owner, factory);

    let outcome = executor.run_next().await.expect("run").expect("task");
    assert_eq!(outcome.state, TransferStateKind::Completed);
    assert_eq!(state.lock().expect("state").upload, b"upload-data");
}

#[test]
fn upload_source_accepts_a_readable_regular_file_from_another_owner() {
    let directory = tempdir().expect("directory");
    let source = directory.path().join("shared-upload.bin");
    std::fs::write(&source, b"shared-data").expect("source");
    let source_uid = std::fs::metadata(&source).expect("metadata").uid();
    let owner = LocalHandleOwner::new(source_uid.wrapping_add(1));

    owner
        .bind_source(LocalFileHandle::new(), source)
        .expect("readable regular source should not require matching ownership");
}

#[test]
fn upload_source_rejects_a_symbolic_link() {
    let directory = tempdir().expect("directory");
    let source = directory.path().join("upload.bin");
    let link = directory.path().join("upload-link.bin");
    std::fs::write(&source, b"data").expect("source");
    symlink(&source, &link).expect("symlink");
    let owner = LocalHandleOwner::default();

    let error = owner
        .bind_source(LocalFileHandle::new(), link)
        .expect_err("symbolic link must remain rejected");
    assert_eq!(error.reason.as_str(), "local_file_unsafe");
}

#[test]
fn upload_source_reports_permission_denied_for_an_unreadable_regular_file() {
    let directory = tempdir().expect("directory");
    let source = directory.path().join("private-upload.bin");
    std::fs::write(&source, b"data").expect("source");
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o000))
        .expect("source permissions");
    let owner = LocalHandleOwner::default();

    let error = owner
        .bind_source(LocalFileHandle::new(), source)
        .expect_err("unreadable regular file must be rejected");
    assert_eq!(error.kind, RemoteErrorKind::PermissionDenied);
    assert_eq!(error.reason.as_str(), "local_file_permission_denied");
}

#[tokio::test]
async fn cancellation_aborts_inflight_download_and_preserves_a_checkpoint() {
    let directory = tempdir().expect("directory");
    let destination = directory.path().join("cancelled.bin");
    let local = LocalFileHandle::new();
    let owner = Arc::new(LocalHandleOwner::default());
    owner.bind_destination(local, &destination).expect("bind");
    let profile_id = ProfileId::new();
    let caps = capabilities(true, false);
    let state = Arc::new(Mutex::new(FakeState {
        download: b"never-committed".to_vec(),
        block_reads: true,
        ..FakeState::default()
    }));
    let opened = Arc::new(Notify::new());
    let factory = Arc::new(FakeFactory {
        profile_id,
        state,
        opened: Arc::clone(&opened),
        capabilities: caps.clone(),
    });
    let task = task(TransferDirection::Download, profile_id, local, 15, &caps);
    let id = task.id;
    let executor = executor(task, owner, factory);
    let runner = {
        let executor = executor.clone();
        tokio::spawn(async move { executor.run_next().await })
    };
    tokio::time::timeout(Duration::from_secs(1), opened.notified())
        .await
        .expect("opened");
    assert_eq!(executor.request_shutdown().expect("shutdown"), 1);
    let outcome = runner.await.expect("join").expect("run").expect("task");
    assert_eq!(outcome.state, TransferStateKind::Cancelled);
    assert!(!destination.exists());
    assert_eq!(
        std::fs::read_dir(directory.path())
            .expect("directory")
            .count(),
        0
    );
    let queue = executor.queue();
    let queue = queue.lock().expect("queue");
    let task = queue.task(id).expect("task");
    assert_eq!(task.state.kind(), TransferStateKind::Cancelled);
    assert_eq!(task.progress.bytes_transferred, 0);
}

#[tokio::test]
async fn upload_failure_aborts_the_remote_staged_write() {
    let directory = tempdir().expect("directory");
    let source = directory.path().join("failed-upload.bin");
    std::fs::write(&source, b"abcdef").expect("source");
    let local = LocalFileHandle::new();
    let owner = Arc::new(LocalHandleOwner::default());
    owner.bind_source(local, source).expect("bind");
    let profile_id = ProfileId::new();
    let caps = capabilities(false, true);
    let state = Arc::new(Mutex::new(FakeState {
        fail_write_after: Some(2),
        ..FakeState::default()
    }));
    let factory = Arc::new(FakeFactory {
        profile_id,
        state: Arc::clone(&state),
        opened: Arc::new(Notify::new()),
        capabilities: caps.clone(),
    });
    let task = task(TransferDirection::Upload, profile_id, local, 6, &caps);
    let executor = executor(task, owner, factory);

    let outcome = executor.run_next().await.expect("run").expect("task");
    assert_eq!(outcome.state, TransferStateKind::Failed);
    let state = state.lock().expect("state");
    assert_eq!(state.upload, b"ab");
    assert_eq!(state.aborts, 1);
    assert!(state.write_handle.is_none());
}

#[test]
fn executor_never_promises_inflight_pause() {
    let directory = tempdir().expect("directory");
    let source = directory.path().join("pause.bin");
    std::fs::write(&source, b"data").expect("source");
    let local = LocalFileHandle::new();
    let owner = Arc::new(LocalHandleOwner::default());
    owner.bind_source(local, source).expect("bind");
    let profile_id = ProfileId::new();
    let caps = capabilities(false, true);
    let factory = Arc::new(FakeFactory {
        profile_id,
        state: Arc::new(Mutex::new(FakeState::default())),
        opened: Arc::new(Notify::new()),
        capabilities: caps.clone(),
    });
    let task = task(TransferDirection::Upload, profile_id, local, 4, &caps);
    assert!(matches!(
        task.features.pause,
        FeatureSupport::Unsupported(_)
    ));
    let id = task.id;
    let executor = executor(task, owner, factory);
    assert!(matches!(
        executor.request_pause(id),
        Err(localdesk_transfers::ExecutorError::PauseUnsupported)
    ));
}

#[tokio::test]
async fn executor_rejects_factories_without_controlled_io_before_open() {
    let directory = tempdir().expect("directory");
    let source = directory.path().join("unsupported.bin");
    std::fs::write(&source, b"data").expect("source");
    let local = LocalFileHandle::new();
    let owner = Arc::new(LocalHandleOwner::default());
    owner.bind_source(local, source).expect("bind");
    let profile_id = ProfileId::new();
    let caps = capabilities(false, true);
    let task = task(TransferDirection::Upload, profile_id, local, 4, &caps);
    let id = task.id;
    let mut queue =
        TransferQueue::open(InMemoryTransferStore::default(), QueueLimits::default(), 1)
            .expect("queue");
    queue.enqueue(task).expect("enqueue");
    let executor = TransferExecutor::new(
        queue,
        owner,
        Arc::new(UnsupportedFactory),
        ExecutorLimits::default(),
    );

    let outcome = executor.run_next().await.expect("run").expect("task");
    assert_eq!(outcome.state, TransferStateKind::Failed);
    let queue = executor.queue();
    let queue = queue.lock().expect("queue");
    let TransferState::Failed { failure } = &queue.task(id).expect("task").state else {
        panic!("task must fail")
    };
    assert_eq!(failure.kind, RemoteErrorKind::Unsupported);
    assert_eq!(failure.reason.as_str(), "fixture_io_control_unsupported");
}

#[tokio::test]
async fn ftp_and_smb_execute_when_the_runtime_factory_reports_file_capabilities() {
    for protocol in [RemoteProtocol::Ftp, RemoteProtocol::Smb] {
        let directory = tempdir().expect("directory");
        let source = directory.path().join("upload.bin");
        std::fs::write(&source, b"data").expect("source");
        let local = LocalFileHandle::new();
        let owner = Arc::new(LocalHandleOwner::default());
        owner.bind_source(local, source).expect("bind");
        let profile_id = ProfileId::new();
        let caps = capabilities(false, true);
        let state = Arc::new(Mutex::new(FakeState::default()));
        let factory = Arc::new(FakeFactory {
            profile_id,
            state: Arc::clone(&state),
            opened: Arc::new(Notify::new()),
            capabilities: caps.clone(),
        });
        let mut task = task(TransferDirection::Upload, profile_id, local, 4, &caps);
        let TransferEndpoint::Remote(endpoint) = &mut task.destination else {
            panic!("upload destination must be remote")
        };
        endpoint.protocol = protocol;
        task.features =
            TransferFeatureSet::from_adapter(TransferDirection::Upload, protocol, &caps);
        let executor = executor(task, owner, factory);

        let outcome = executor.run_next().await.expect("run").expect("task");
        assert_eq!(outcome.state, TransferStateKind::Completed);
        let observed = state.lock().expect("state");
        assert_eq!(observed.opens, 1);
        assert_eq!(observed.upload, b"data");
        assert_eq!(observed.disconnects, 1);
    }
}

#[tokio::test]
async fn capability_rejection_still_uses_controlled_disconnect() {
    let directory = tempdir().expect("directory");
    let source = directory.path().join("no-write.bin");
    std::fs::write(&source, b"data").expect("source");
    let local = LocalFileHandle::new();
    let owner = Arc::new(LocalHandleOwner::default());
    owner.bind_source(local, source).expect("bind");
    let profile_id = ProfileId::new();
    let caps = capabilities(false, false);
    let state = Arc::new(Mutex::new(FakeState::default()));
    let factory = Arc::new(FakeFactory {
        profile_id,
        state: Arc::clone(&state),
        opened: Arc::new(Notify::new()),
        capabilities: caps.clone(),
    });
    let task = task(TransferDirection::Upload, profile_id, local, 4, &caps);
    let executor = executor(task, owner, factory);

    let outcome = executor.run_next().await.expect("run").expect("task");
    assert_eq!(outcome.state, TransferStateKind::Failed);
    let state = state.lock().expect("state");
    assert_eq!(state.opens, 1);
    assert_eq!(state.disconnects, 1);
}

#[tokio::test]
async fn operation_deadline_schedules_a_bounded_retry_and_aborts_local_stage() {
    let directory = tempdir().expect("directory");
    let destination = directory.path().join("timeout.bin");
    let local = LocalFileHandle::new();
    let owner = Arc::new(LocalHandleOwner::default());
    owner.bind_destination(local, &destination).expect("bind");
    let profile_id = ProfileId::new();
    let caps = capabilities(true, false);
    let state = Arc::new(Mutex::new(FakeState {
        download: b"blocked".to_vec(),
        block_reads: true,
        ..FakeState::default()
    }));
    let factory = Arc::new(FakeFactory {
        profile_id,
        state,
        opened: Arc::new(Notify::new()),
        capabilities: caps.clone(),
    });
    let task = task(TransferDirection::Download, profile_id, local, 7, &caps);
    let mut queue =
        TransferQueue::open(InMemoryTransferStore::default(), QueueLimits::default(), 1)
            .expect("queue");
    queue.enqueue(task).expect("enqueue");
    let executor = TransferExecutor::new(
        queue,
        owner,
        factory,
        ExecutorLimits::new(1, 2, Duration::from_millis(15), Duration::from_millis(100))
            .expect("limits"),
    );

    let outcome = executor.run_next().await.expect("run").expect("task");
    assert_eq!(outcome.state, TransferStateKind::RetryScheduled);
    assert!(!destination.exists());
    assert_eq!(
        std::fs::read_dir(directory.path())
            .expect("directory")
            .count(),
        0
    );
}

#[tokio::test]
async fn run_ready_obeys_executor_concurrency_independently_of_queue_capacity() {
    let directory = tempdir().expect("directory");
    let first = LocalFileHandle::new();
    let second = LocalFileHandle::new();
    let owner = Arc::new(LocalHandleOwner::default());
    owner
        .bind_destination(first, directory.path().join("first.bin"))
        .expect("bind first");
    owner
        .bind_destination(second, directory.path().join("second.bin"))
        .expect("bind second");
    let profile_id = ProfileId::new();
    let caps = capabilities(true, false);
    let state = Arc::new(Mutex::new(FakeState {
        download: b"x".to_vec(),
        block_reads: true,
        ..FakeState::default()
    }));
    let opened = Arc::new(Notify::new());
    let factory = Arc::new(FakeFactory {
        profile_id,
        state: Arc::clone(&state),
        opened: Arc::clone(&opened),
        capabilities: caps.clone(),
    });
    let mut queue =
        TransferQueue::open(InMemoryTransferStore::default(), QueueLimits::default(), 1)
            .expect("queue");
    queue
        .enqueue(task(
            TransferDirection::Download,
            profile_id,
            first,
            1,
            &caps,
        ))
        .expect("first task");
    queue
        .enqueue(task(
            TransferDirection::Download,
            profile_id,
            second,
            1,
            &caps,
        ))
        .expect("second task");
    let executor = TransferExecutor::new(
        queue,
        owner,
        factory,
        ExecutorLimits::new(1, 1, Duration::from_secs(1), Duration::from_secs(5)).expect("limits"),
    );
    let runner = {
        let executor = executor.clone();
        tokio::spawn(async move { executor.run_ready().await })
    };

    tokio::time::timeout(Duration::from_secs(1), opened.notified())
        .await
        .expect("first open");
    assert_eq!(state.lock().expect("state").opens, 1);
    state.lock().expect("state").block_reads = false;
    let outcomes = runner.await.expect("join").expect("run");
    assert_eq!(outcomes.len(), 2);
    assert!(
        outcomes
            .iter()
            .all(|outcome| outcome.state == TransferStateKind::Completed)
    );
    assert_eq!(state.lock().expect("state").opens, 2);
}
