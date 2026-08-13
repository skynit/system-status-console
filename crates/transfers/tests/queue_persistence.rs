use localdesk_remote_core::{
    CapabilityMatrix, CapabilityStatus, FILE_OPERATIONS, FileOperation, ObjectIdentity,
    OperationCapability, ProfileId, RemotePath, RemoteProtocol, SafeReason,
};
use localdesk_transfers::{
    BandwidthLimit, ConflictPolicy, InMemoryTransferStore, LocalFileHandle, QueueLimits,
    RetryPolicy, SQLITE_BEGIN_WRITE, SQLITE_COMPARE_AND_SWAP_TASK, SQLITE_SOLE_WRITER_RULES,
    SQLITE_TRANSFER_SCHEMA_V1, SQLITE_TRANSFER_SCHEMA_VERSION, TransferDirection, TransferEndpoint,
    TransferFeatureSet, TransferId, TransferQueue, TransferState, TransferTask,
};

fn matrix() -> CapabilityMatrix {
    CapabilityMatrix::complete(FILE_OPERATIONS.iter().copied().map(|operation| {
        OperationCapability {
            operation,
            status: if matches!(
                operation,
                FileOperation::ResumeRead | FileOperation::ResumeWrite
            ) {
                CapabilityStatus::Supported
            } else {
                CapabilityStatus::Unsupported(SafeReason::new("not_needed_for_fixture").unwrap())
            },
        }
    }))
    .unwrap()
}

fn task(id: TransferId, profile_id: ProfileId, created: i64) -> TransferTask {
    let direction = TransferDirection::Download;
    TransferTask::new(
        id,
        TransferEndpoint::Remote(localdesk_transfers::RemoteTransferEndpoint {
            profile_id,
            protocol: RemoteProtocol::Sftp,
            path: RemotePath::new("/fixture.bin").unwrap(),
        }),
        TransferEndpoint::Local {
            handle: LocalFileHandle::new(),
        },
        direction,
        Some(ObjectIdentity {
            size_bytes: Some(10),
            modified_at_unix_ms: None,
            etag: None,
        }),
        None,
        RetryPolicy::default(),
        BandwidthLimit::unlimited(),
        ConflictPolicy::Fail,
        TransferFeatureSet::from_adapter(direction, RemoteProtocol::Sftp, &matrix()),
        created,
    )
    .unwrap()
}

#[test]
fn sole_writer_queue_persists_each_revision_before_publishing() {
    let id = TransferId::new();
    let profile = ProfileId::new();
    let mut queue =
        TransferQueue::open(InMemoryTransferStore::default(), QueueLimits::default(), 1).unwrap();
    queue.enqueue(task(id, profile, 1)).unwrap();
    let token = queue.start_next(2).unwrap().unwrap();
    queue
        .mutate(id, |task| {
            task.record_progress(token, 5, Some(10), Some(2), 3)
        })
        .unwrap();
    let revision = queue.task(id).unwrap().revision;
    let store = queue.into_store();
    assert_eq!(store.task(id).unwrap().revision, revision);
    assert_eq!(store.task(id).unwrap().progress.bytes_transferred, 5);
}

#[test]
fn restart_does_not_claim_an_active_task_completed() {
    let id = TransferId::new();
    let profile = ProfileId::new();
    let mut active = task(id, profile, 1);
    active.start(2).unwrap();
    let store = InMemoryTransferStore::with_tasks([active]);

    let queue = TransferQueue::open(store, QueueLimits::default(), 3).unwrap();
    assert!(matches!(
        queue.task(id).unwrap().state,
        TransferState::Failed { .. }
    ));
    let store = queue.into_store();
    assert!(matches!(
        store.task(id).unwrap().state,
        TransferState::Failed { .. }
    ));
}

#[test]
fn scheduler_enforces_total_and_per_profile_backpressure() {
    let profile = ProfileId::from_uuid(uuid::Uuid::from_u128(1));
    let other_profile = ProfileId::from_uuid(uuid::Uuid::from_u128(2));
    let first = TransferId::from_uuid(uuid::Uuid::from_u128(11));
    let second = TransferId::from_uuid(uuid::Uuid::from_u128(12));
    let third = TransferId::from_uuid(uuid::Uuid::from_u128(13));
    let limits = QueueLimits::new(10, 2, 1).unwrap();
    let mut queue = TransferQueue::open(InMemoryTransferStore::default(), limits, 1).unwrap();
    queue.enqueue(task(first, profile, 1)).unwrap();
    queue.enqueue(task(second, profile, 2)).unwrap();
    queue.enqueue(task(third, other_profile, 3)).unwrap();

    let first_run = queue.start_next(4).unwrap().unwrap();
    assert_eq!(first_run.task_id, first);
    let second_run = queue.start_next(4).unwrap().unwrap();
    assert_eq!(second_run.task_id, third);
    assert!(queue.start_next(4).unwrap().is_none());
}

#[test]
fn scheduler_is_fifo_even_when_ids_sort_differently() {
    let earlier_id = TransferId::from_uuid(
        uuid::Uuid::parse_str("ffffffff-ffff-ffff-ffff-ffffffffffff").unwrap(),
    );
    let later_id = TransferId::from_uuid(
        uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap(),
    );
    let mut queue =
        TransferQueue::open(InMemoryTransferStore::default(), QueueLimits::default(), 1).unwrap();
    queue
        .enqueue(task(earlier_id, ProfileId::new(), 1))
        .unwrap();
    queue.enqueue(task(later_id, ProfileId::new(), 2)).unwrap();

    assert_eq!(queue.start_next(3).unwrap().unwrap().task_id, earlier_id);
}

#[test]
fn sqlite_contract_freezes_schema_cas_and_recovery_rules() {
    assert_eq!(SQLITE_TRANSFER_SCHEMA_VERSION, 1);
    assert!(SQLITE_TRANSFER_SCHEMA_V1.contains("CREATE TABLE IF NOT EXISTS transfer_tasks"));
    assert!(SQLITE_TRANSFER_SCHEMA_V1.contains("STRICT"));
    assert_eq!(SQLITE_BEGIN_WRITE, "BEGIN IMMEDIATE");
    assert!(SQLITE_COMPARE_AND_SWAP_TASK.contains("AND revision = ?6"));
    assert!(
        SQLITE_SOLE_WRITER_RULES
            .iter()
            .any(|rule| rule.contains("only writable SQLite connection"))
    );
    assert!(
        SQLITE_SOLE_WRITER_RULES
            .iter()
            .any(|rule| rule.contains("without silent rebuild"))
    );
}
