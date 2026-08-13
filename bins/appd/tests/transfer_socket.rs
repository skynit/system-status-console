#[allow(dead_code)]
#[path = "../src/network.rs"]
mod network;
#[allow(dead_code)]
#[path = "../src/notes.rs"]
mod notes;
#[allow(dead_code)]
#[path = "../src/remote.rs"]
mod remote;
#[path = "../src/service.rs"]
mod service;
#[allow(dead_code)]
#[path = "../src/usage.rs"]
mod usage;
#[allow(dead_code)]
#[path = "../src/speedtest.rs"]
mod speedtest;

use speedtest::SpeedTestHandle;
use localdesk_ipc::{
    ClientError, RequestEnvelope, TransferLocalHandleBind, request_health, request_remote_profile,
    request_transfer, request_transfer_local_handle,
};
use localdesk_network::NetworkMonitor;
use localdesk_remote_core::{
    AdapterAvailability, AdapterFuture, Authentication, CapabilityMatrix, CapabilityStatus,
    FILE_OPERATIONS, FileOperation, FirstUsePolicy, OperationCapability, ProfileId, ProfileOptions,
    RemoteConnectionProfile, RemoteEndpoint, RemoteError, RemoteErrorKind, RemoteFileAdapter,
    RemoteFileSession, RemoteIoControl, RemoteIoControlSupport, RemoteOperation, RemotePath,
    RemoteProfileCommand, RemoteProtocol, RetryDisposition, SafeReason, SecretStore, SmbDialect,
    TrustPolicy,
};
use localdesk_telemetry::TelemetryManager;
use localdesk_transfers::{
    BandwidthLimit, ConflictPolicy, LocalFileHandle, QueueLimits, RemoteTransferEndpoint,
    RetryPolicy, SqliteTransferStore, TransferCommand, TransferConflict, TransferDirection,
    TransferDraft, TransferDraftEndpoint, TransferEndpoint, TransferFailure, TransferFeatureSet,
    TransferId, TransferLocalHandlePurpose, TransferMutationResult, TransferOutput, TransferQuery,
    TransferQueue, TransferStateKind, TransferStore, TransferTask,
};
use network::NetworkSupervisor;
use remote::RemoteRuntime;
use std::{
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{Arc, Condvar, Mutex},
};
use tempfile::{TempDir, tempdir};
use tokio::{net::UnixListener, sync::watch, task::JoinHandle};
use usage::UsageHandle;

fn reason(value: &'static str) -> SafeReason {
    SafeReason::new(value).expect("static reason")
}

fn capabilities() -> CapabilityMatrix {
    CapabilityMatrix::complete(FILE_OPERATIONS.iter().copied().map(|operation| {
        OperationCapability {
            operation,
            status: if matches!(
                operation,
                FileOperation::Read
                    | FileOperation::Write
                    | FileOperation::ResumeRead
                    | FileOperation::ResumeWrite
            ) {
                CapabilityStatus::Supported
            } else {
                CapabilityStatus::Unsupported(reason("fixture_operation_unsupported"))
            },
        }
    }))
    .expect("complete capabilities")
}

struct LocalOnlySftpAdapter {
    capabilities: CapabilityMatrix,
    capability_gate: Option<Arc<BlockingCapabilityGate>>,
}

impl LocalOnlySftpAdapter {
    fn new() -> Self {
        Self {
            capabilities: capabilities(),
            capability_gate: None,
        }
    }

    fn gated(gate: Arc<BlockingCapabilityGate>) -> Self {
        Self {
            capabilities: capabilities(),
            capability_gate: Some(gate),
        }
    }

    fn rejected_connect<'a>() -> AdapterFuture<'a, Result<Box<dyn RemoteFileSession>, RemoteError>>
    {
        Box::pin(async {
            Err(RemoteError::new(
                RemoteErrorKind::Transport,
                RemoteOperation::Connect,
                reason("fixture_connect_disabled"),
                RetryDisposition::Backoff,
            ))
        })
    }
}

impl RemoteFileAdapter for LocalOnlySftpAdapter {
    fn protocol(&self) -> RemoteProtocol {
        RemoteProtocol::Sftp
    }

    fn availability(&self) -> AdapterAvailability {
        AdapterAvailability::Healthy
    }

    fn capabilities(&self) -> &CapabilityMatrix {
        if let Some(gate) = &self.capability_gate {
            gate.block();
        }
        &self.capabilities
    }

    fn io_control_support(&self) -> RemoteIoControlSupport {
        RemoteIoControlSupport::Supported
    }

    fn connect<'a>(
        &'a self,
        _profile: &'a RemoteConnectionProfile,
        _secrets: &'a dyn SecretStore,
    ) -> AdapterFuture<'a, Result<Box<dyn RemoteFileSession>, RemoteError>> {
        Self::rejected_connect()
    }

    fn connect_controlled<'a>(
        &'a self,
        _profile: &'a RemoteConnectionProfile,
        _secrets: &'a dyn SecretStore,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<Box<dyn RemoteFileSession>, RemoteError>> {
        Box::pin(async move {
            control.check(RemoteOperation::Connect)?;
            Self::rejected_connect().await
        })
    }
}

struct BlockingCapabilityGate {
    released: Mutex<bool>,
    changed: Condvar,
    entered: tokio::sync::Notify,
}

impl BlockingCapabilityGate {
    fn new() -> Self {
        Self {
            released: Mutex::new(false),
            changed: Condvar::new(),
            entered: tokio::sync::Notify::new(),
        }
    }

    fn block(&self) {
        self.entered.notify_one();
        let released = self
            .released
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _released = self
            .changed
            .wait_while(released, |released| !*released)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
    }

    fn release(&self) {
        *self
            .released
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
        self.changed.notify_all();
    }
}

struct AppdFixture {
    _socket_directory: TempDir,
    path: PathBuf,
    shutdown: watch::Sender<bool>,
    server: JoinHandle<Result<(), localdesk_ipc::ServerError>>,
}

impl AppdFixture {
    async fn spawn(remote: RemoteRuntime) -> Self {
        let socket_directory = tempdir().expect("socket directory");
        let path = socket_directory.path().join("appd.sock");
        let listener = UnixListener::bind(&path).expect("listener");
        let telemetry = TelemetryManager::with_defaults();
        let network = NetworkSupervisor::new(NetworkMonitor::default());
        let (shutdown, shutdown_rx) = watch::channel(false);
        let server = tokio::spawn(service::serve_appd(
            listener,
            telemetry.handle(),
            network.handle(),
            UsageHandle::unavailable_for_test("usage_fixture_unavailable"),
            notes::NotesHandle::unavailable_for_test(),
            remote,
            SpeedTestHandle::new(),
            shutdown_rx,
        ));
        Self {
            _socket_directory: socket_directory,
            path,
            shutdown,
            server,
        }
    }

    async fn stop(self) {
        self.shutdown.send(true).expect("shutdown");
        self.server.await.expect("server join").expect("serve");
    }
}

fn sftp_profile(id: ProfileId) -> RemoteConnectionProfile {
    RemoteConnectionProfile::new(
        id,
        "fixture sftp",
        RemoteProtocol::Sftp,
        RemoteEndpoint::new("fixture.invalid", 22).expect("endpoint"),
        Some("operator".to_owned()),
        None,
        Authentication::SshAgent,
        TrustPolicy::SshKnownHosts {
            first_use: FirstUsePolicy::Reject,
        },
        ProfileOptions::Sftp {
            jump_profiles: Vec::new(),
        },
    )
    .expect("sftp profile")
}

fn smb_profile(id: ProfileId) -> RemoteConnectionProfile {
    RemoteConnectionProfile::new(
        id,
        "fixture smb",
        RemoteProtocol::Smb,
        RemoteEndpoint::new("fixture.invalid", 445).expect("endpoint"),
        None,
        None,
        Authentication::Kerberos,
        TrustPolicy::SmbNegotiated,
        ProfileOptions::Smb {
            share: Some("share".to_owned()),
            minimum_dialect: SmbDialect::Smb3,
            require_signing: true,
            require_encryption: true,
        },
    )
    .expect("smb profile")
}

fn draft(id: TransferId, profile_id: ProfileId, handle: LocalFileHandle) -> TransferDraft {
    TransferDraft {
        id,
        source: TransferDraftEndpoint::Local { handle },
        destination: TransferDraftEndpoint::Remote {
            profile_id,
            path: RemotePath::new("/destination.bin").expect("remote path"),
        },
        direction: TransferDirection::Upload,
        expected_source: None,
        expected_destination: None,
        retry_policy: RetryPolicy::default(),
        bandwidth_limit: BandwidthLimit::unlimited(),
        conflict_policy: ConflictPolicy::Fail,
    }
}

fn stored_task(id: TransferId, profile_id: ProfileId, handle: LocalFileHandle) -> TransferTask {
    TransferTask::new(
        id,
        TransferEndpoint::Local { handle },
        TransferEndpoint::Remote(RemoteTransferEndpoint {
            profile_id,
            protocol: RemoteProtocol::Sftp,
            path: RemotePath::new("/seeded.bin").expect("remote path"),
        }),
        TransferDirection::Upload,
        None,
        None,
        RetryPolicy::default(),
        BandwidthLimit::unlimited(),
        ConflictPolicy::Fail,
        TransferFeatureSet::from_adapter(
            TransferDirection::Upload,
            RemoteProtocol::Sftp,
            &capabilities(),
        ),
        1,
    )
    .expect("task")
}

fn seed_mutation_tasks(
    database: &std::path::Path,
    profile_id: ProfileId,
) -> (TransferId, TransferId) {
    let store = SqliteTransferStore::open(database).expect("transfer store");
    let mut queue = TransferQueue::open(
        store,
        QueueLimits::new(1_000, 4, 2).expect("queue limits"),
        1,
    )
    .expect("queue");

    let retry_id = TransferId::new();
    queue
        .enqueue(stored_task(retry_id, profile_id, LocalFileHandle::new()))
        .expect("retry seed");
    let retry_token = queue
        .start_next(2)
        .expect("start retry seed")
        .expect("token");
    queue
        .mutate(retry_id, |task| {
            task.fail(
                retry_token,
                TransferFailure {
                    kind: RemoteErrorKind::Transport,
                    operation: RemoteOperation::Connect,
                    reason: reason("fixture_retryable_failure"),
                    retry: RetryDisposition::Backoff,
                },
                3,
            )
        })
        .expect("fail retry seed");

    let conflict_id = TransferId::new();
    queue
        .enqueue(stored_task(conflict_id, profile_id, LocalFileHandle::new()))
        .expect("conflict seed");
    let conflict_token = queue
        .start_next(4)
        .expect("start conflict seed")
        .expect("token");
    queue
        .mutate(conflict_id, |task| {
            task.enter_conflict(
                conflict_token,
                TransferConflict {
                    reason: reason("fixture_conflict"),
                    checkpoint: None,
                },
                5,
            )
        })
        .expect("conflict seed state");
    drop(queue.into_store());
    (retry_id, conflict_id)
}

async fn store_profile(runtime: &RemoteRuntime, profile: RemoteConnectionProfile) {
    runtime
        .profile_command(RemoteProfileCommand::Upsert {
            profile,
            expected_revision: None,
        })
        .await
        .expect("store profile");
}

async fn bind_upload_source(socket: &std::path::Path, source: PathBuf) -> LocalFileHandle {
    request_transfer_local_handle(
        socket,
        RequestEnvelope::transfer_local_handle(TransferLocalHandleBind {
            purpose: TransferLocalHandlePurpose::UploadSource,
            path: source,
        }),
    )
    .await
    .expect("production opaque handle binding")
    .handle
}

fn daemon_error(error: ClientError) -> localdesk_ipc::DaemonError {
    let ClientError::Daemon(error) = error else {
        panic!("expected daemon error, got {error:?}")
    };
    error
}

#[tokio::test]
async fn provider_reports_runner_not_started_and_shutdown_truthfully() {
    let state = tempdir().expect("state directory");
    std::fs::set_permissions(state.path(), std::fs::Permissions::from_mode(0o700))
        .expect("state permissions");
    let runtime = RemoteRuntime::from_state_base_for_test(state.path());
    let fixture = AppdFixture::spawn(runtime.clone()).await;

    let error = request_transfer(
        &fixture.path,
        RequestEnvelope::transfer(TransferCommand::List {
            query: TransferQuery {
                limit: 1,
                offset: 0,
                states: Vec::new(),
                direction: None,
                profile_id: None,
            },
        }),
    )
    .await
    .expect_err("inactive runner");
    let error = daemon_error(error);
    assert_eq!(error.code, "transfer_provider_unavailable");
    assert_eq!(error.reason, "transfer_runner_not_active");
    assert!(error.retryable);

    runtime.start_transfer_runner().await;
    runtime.shutdown_sessions().await;
    let error = request_transfer(
        &fixture.path,
        RequestEnvelope::transfer(TransferCommand::List {
            query: TransferQuery {
                limit: 1,
                offset: 0,
                states: Vec::new(),
                direction: None,
                profile_id: None,
            },
        }),
    )
    .await
    .expect_err("stopped runner");
    let error = daemon_error(error);
    assert_eq!(error.code, "transfer_provider_unavailable");
    assert_eq!(error.reason, "transfer_runner_not_active");
    fixture.stop().await;
}

#[tokio::test]
async fn public_commands_use_the_live_runner_cas_and_opaque_handles() {
    let state = tempdir().expect("state directory");
    std::fs::set_permissions(state.path(), std::fs::Permissions::from_mode(0o700))
        .expect("state permissions");
    let profile_id = ProfileId::new();
    let smb_profile_id = ProfileId::new();
    let runtime = RemoteRuntime::from_state_base_for_test(state.path())
        .with_file_adapter_for_test(Arc::new(LocalOnlySftpAdapter::new()));
    store_profile(&runtime, sftp_profile(profile_id)).await;
    store_profile(&runtime, smb_profile(smb_profile_id)).await;
    let database = state.path().join("localdesk/transfers.sqlite3");
    let (retry_id, conflict_id) = seed_mutation_tasks(&database, profile_id);

    let source = state.path().join("source.bin");
    std::fs::write(&source, b"local-only fixture").expect("source fixture");
    runtime.start_transfer_runner().await;
    let fixture = AppdFixture::spawn(runtime.clone()).await;
    let grant = request_transfer_local_handle(
        &fixture.path,
        RequestEnvelope::transfer_local_handle(TransferLocalHandleBind {
            purpose: TransferLocalHandlePurpose::UploadSource,
            path: source.clone(),
        }),
    )
    .await
    .expect("production opaque handle binding");
    assert_eq!(grant.display_name, "source.bin");
    assert_eq!(grant.size_bytes, Some(18));
    assert!(
        serde_json::to_value(&grant)
            .expect("grant json")
            .get("path")
            .is_none()
    );
    let handle = grant.handle;
    let destination = state.path().join("download.bin");
    let destination_grant = request_transfer_local_handle(
        &fixture.path,
        RequestEnvelope::transfer_local_handle(TransferLocalHandleBind {
            purpose: TransferLocalHandlePurpose::DownloadDestination,
            path: destination,
        }),
    )
    .await
    .expect("production opaque destination binding");
    assert_eq!(destination_grant.display_name, "download.bin");
    assert_eq!(destination_grant.size_bytes, None);
    assert!(
        serde_json::to_value(&destination_grant)
            .expect("destination grant json")
            .get("path")
            .is_none()
    );

    let health = request_health(
        &fixture.path,
        RequestEnvelope::health(
            "fixture",
            vec![localdesk_domain::TRANSFERS_CAPABILITY.to_owned()],
        ),
    )
    .await
    .expect("health");
    let transfer_capability = health
        .capabilities
        .iter()
        .find(|capability| capability.id == localdesk_domain::TRANSFERS_CAPABILITY)
        .expect("transfer capability");
    assert_eq!(
        transfer_capability.status,
        localdesk_domain::CapabilityAvailability::Healthy
    );
    assert_eq!(
        transfer_capability.reason,
        "transfer_runner_active_public_commands_available"
    );

    let live_id = TransferId::new();
    let enqueued = request_transfer(
        &fixture.path,
        RequestEnvelope::transfer(TransferCommand::Enqueue {
            draft: draft(live_id, profile_id, handle),
        }),
    )
    .await
    .expect("enqueue");
    assert!(matches!(enqueued, TransferOutput::Task { task } if task.id == live_id));

    let page = request_transfer(
        &fixture.path,
        RequestEnvelope::transfer(TransferCommand::List {
            query: TransferQuery {
                limit: 64,
                offset: 0,
                states: Vec::new(),
                direction: None,
                profile_id: Some(profile_id),
            },
        }),
    )
    .await
    .expect("list");
    assert!(matches!(page, TransferOutput::Page { page } if page.tasks.len() == 3));

    let current = request_transfer(
        &fixture.path,
        RequestEnvelope::transfer(TransferCommand::Get { id: live_id }),
    )
    .await
    .expect("get");
    let TransferOutput::Task { task: current } = current else {
        panic!("get task output")
    };

    let stale = request_transfer(
        &fixture.path,
        RequestEnvelope::transfer(TransferCommand::Cancel {
            id: live_id,
            expected_revision: u64::MAX,
        }),
    )
    .await
    .expect("typed stale conflict");
    assert!(matches!(
        stale,
        TransferOutput::Mutation {
            result: TransferMutationResult::Conflict {
                expected_revision: u64::MAX,
                current: ref conflict_current,
            },
        } if conflict_current.id == live_id
    ));

    let cancelled = request_transfer(
        &fixture.path,
        RequestEnvelope::transfer(TransferCommand::Cancel {
            id: live_id,
            expected_revision: current.revision,
        }),
    )
    .await;
    match cancelled {
        Ok(TransferOutput::Mutation {
            result: TransferMutationResult::Updated { task },
        }) => assert!(matches!(
            task.state.kind(),
            TransferStateKind::Cancelling | TransferStateKind::Cancelled
        )),
        Ok(TransferOutput::Mutation {
            result: TransferMutationResult::Conflict { current, .. },
        }) => {
            let retried = request_transfer(
                &fixture.path,
                RequestEnvelope::transfer(TransferCommand::Cancel {
                    id: live_id,
                    expected_revision: current.revision,
                }),
            )
            .await
            .expect("cancel after racing runner revision");
            assert!(matches!(
                retried,
                TransferOutput::Mutation {
                    result: TransferMutationResult::Updated { .. }
                }
            ));
        }
        other => panic!("unexpected cancel result: {other:?}"),
    }

    let retry_current = request_transfer(
        &fixture.path,
        RequestEnvelope::transfer(TransferCommand::Get { id: retry_id }),
    )
    .await
    .expect("get retry seed");
    let TransferOutput::Task {
        task: retry_current,
    } = retry_current
    else {
        panic!("retry task output")
    };
    let retried = request_transfer(
        &fixture.path,
        RequestEnvelope::transfer(TransferCommand::Retry {
            id: retry_id,
            expected_revision: retry_current.revision,
        }),
    )
    .await
    .expect("retry");
    assert!(matches!(
        retried,
        TransferOutput::Mutation {
            result: TransferMutationResult::Updated { task }
        } if task.state.kind() == TransferStateKind::RetryScheduled
    ));

    let conflict_current = request_transfer(
        &fixture.path,
        RequestEnvelope::transfer(TransferCommand::Get { id: conflict_id }),
    )
    .await
    .expect("get conflict seed");
    let TransferOutput::Task {
        task: conflict_current,
    } = conflict_current
    else {
        panic!("conflict task output")
    };
    let resolved = request_transfer(
        &fixture.path,
        RequestEnvelope::transfer(TransferCommand::ResolveConflict {
            id: conflict_id,
            expected_revision: conflict_current.revision,
            policy: ConflictPolicy::Overwrite,
        }),
    )
    .await
    .expect("resolve conflict");
    assert!(matches!(
        resolved,
        TransferOutput::Mutation {
            result: TransferMutationResult::Updated { task }
        } if task.state.kind() == TransferStateKind::Queued
    ));

    let unbound = request_transfer(
        &fixture.path,
        RequestEnvelope::transfer(TransferCommand::Enqueue {
            draft: draft(TransferId::new(), profile_id, LocalFileHandle::new()),
        }),
    )
    .await
    .expect_err("unbound opaque handle");
    let unbound = daemon_error(unbound);
    assert_eq!(unbound.code, "transfer_local_handle_unavailable");
    assert_eq!(unbound.reason, "local_handle_not_bound");

    let smb_id = TransferId::new();
    let smb = request_transfer(
        &fixture.path,
        RequestEnvelope::transfer(TransferCommand::Enqueue {
            draft: draft(smb_id, smb_profile_id, handle),
        }),
    )
    .await
    .expect("SMB adapter accepts a bounded transfer task");
    assert!(matches!(smb, TransferOutput::Task { task } if task.id == smb_id));

    runtime.shutdown_sessions().await;
    fixture.stop().await;
    let store = SqliteTransferStore::open(&database).expect("reopen sole-writer store");
    let persisted = store.load_all().expect("persisted tasks");
    assert_eq!(persisted.len(), 4);
    assert!(persisted.iter().any(|task| task.id == live_id));
    assert!(persisted.iter().any(|task| task.id == retry_id));
    assert!(persisted.iter().any(|task| task.id == conflict_id));
    assert!(persisted.iter().any(|task| task.id == smb_id));
    drop(store);

    let restarted = RemoteRuntime::from_state_base_for_test(state.path())
        .with_file_adapter_for_test(Arc::new(LocalOnlySftpAdapter::new()));
    restarted.start_transfer_runner().await;
    let restarted_fixture = AppdFixture::spawn(restarted.clone()).await;
    let stale_handle = request_transfer(
        &restarted_fixture.path,
        RequestEnvelope::transfer(TransferCommand::Enqueue {
            draft: draft(TransferId::new(), profile_id, handle),
        }),
    )
    .await
    .expect_err("restart requires a fresh native selection");
    assert_eq!(daemon_error(stale_handle).reason, "local_handle_not_bound");

    let rebound = request_transfer_local_handle(
        &restarted_fixture.path,
        RequestEnvelope::transfer_local_handle(TransferLocalHandleBind {
            purpose: TransferLocalHandlePurpose::UploadSource,
            path: source,
        }),
    )
    .await
    .expect("rebind after restart");
    assert_ne!(rebound.handle, handle);
    request_transfer(
        &restarted_fixture.path,
        RequestEnvelope::transfer(TransferCommand::Enqueue {
            draft: draft(TransferId::new(), profile_id, rebound.handle),
        }),
    )
    .await
    .expect("enqueue after explicit rebind");
    restarted.shutdown_sessions().await;
    restarted_fixture.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn enqueue_and_profile_delete_are_serialized_against_the_live_queue() {
    let state = tempdir().expect("state directory");
    std::fs::set_permissions(state.path(), std::fs::Permissions::from_mode(0o700))
        .expect("state permissions");
    let profile_id = ProfileId::new();
    let gate = Arc::new(BlockingCapabilityGate::new());
    let runtime = RemoteRuntime::from_state_base_for_test(state.path())
        .with_file_adapter_for_test(Arc::new(LocalOnlySftpAdapter::gated(gate.clone())));
    store_profile(&runtime, sftp_profile(profile_id)).await;
    runtime.start_transfer_runner().await;
    let fixture = AppdFixture::spawn(runtime.clone()).await;
    let source = state.path().join("delete-race-source.bin");
    std::fs::write(&source, b"local-only fixture").expect("source fixture");
    let handle = bind_upload_source(&fixture.path, source).await;

    let enqueue_path = fixture.path.clone();
    let enqueue = tokio::spawn(async move {
        request_transfer(
            &enqueue_path,
            RequestEnvelope::transfer(TransferCommand::Enqueue {
                draft: draft(TransferId::new(), profile_id, handle),
            }),
        )
        .await
    });
    gate.entered.notified().await;

    let delete_path = fixture.path.clone();
    let mut delete = tokio::spawn(async move {
        request_remote_profile(
            &delete_path,
            RequestEnvelope::remote_profile(RemoteProfileCommand::Delete {
                profile_id,
                expected_revision: 0,
            }),
        )
        .await
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut delete)
            .await
            .is_err(),
        "delete must wait while enqueue owns the profile-transfer gate"
    );

    gate.release();
    assert!(matches!(
        enqueue.await.expect("enqueue join").expect("enqueue"),
        TransferOutput::Task { .. }
    ));
    let error = delete
        .await
        .expect("delete join")
        .expect_err("live transfer prevents profile deletion");
    let error = daemon_error(error);
    assert_eq!(error.code, "remote_profile_in_use");
    assert_eq!(error.reason, "remote_profile_in_use");

    runtime.shutdown_sessions().await;
    fixture.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn enqueue_and_protocol_change_are_serialized_against_the_live_queue() {
    let state = tempdir().expect("state directory");
    std::fs::set_permissions(state.path(), std::fs::Permissions::from_mode(0o700))
        .expect("state permissions");
    let profile_id = ProfileId::new();
    let gate = Arc::new(BlockingCapabilityGate::new());
    let runtime = RemoteRuntime::from_state_base_for_test(state.path())
        .with_file_adapter_for_test(Arc::new(LocalOnlySftpAdapter::gated(gate.clone())));
    store_profile(&runtime, sftp_profile(profile_id)).await;
    runtime.start_transfer_runner().await;
    let fixture = AppdFixture::spawn(runtime.clone()).await;
    let source = state.path().join("protocol-race-source.bin");
    std::fs::write(&source, b"local-only fixture").expect("source fixture");
    let handle = bind_upload_source(&fixture.path, source).await;

    let enqueue_path = fixture.path.clone();
    let enqueue = tokio::spawn(async move {
        request_transfer(
            &enqueue_path,
            RequestEnvelope::transfer(TransferCommand::Enqueue {
                draft: draft(TransferId::new(), profile_id, handle),
            }),
        )
        .await
    });
    gate.entered.notified().await;

    let upsert_path = fixture.path.clone();
    let mut upsert = tokio::spawn(async move {
        request_remote_profile(
            &upsert_path,
            RequestEnvelope::remote_profile(RemoteProfileCommand::Upsert {
                profile: smb_profile(profile_id),
                expected_revision: Some(0),
            }),
        )
        .await
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut upsert)
            .await
            .is_err(),
        "protocol change must wait while enqueue owns the profile-transfer gate"
    );

    gate.release();
    assert!(matches!(
        enqueue.await.expect("enqueue join").expect("enqueue"),
        TransferOutput::Task { .. }
    ));
    let error = upsert
        .await
        .expect("upsert join")
        .expect_err("live transfer prevents protocol change");
    let error = daemon_error(error);
    assert_eq!(error.code, "remote_profile_in_use");
    assert_eq!(error.reason, "remote_profile_protocol_change_in_use");

    runtime.shutdown_sessions().await;
    fixture.stop().await;
}
