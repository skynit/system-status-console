use localdesk_remote_core::{ProfileId, RemotePath, RemoteProtocol, SafeReason};
use localdesk_transfers::{
    BandwidthLimit, ConflictPolicy, FeatureSupport, LocalFileHandle, QueueLimits,
    RemoteTransferEndpoint, RetryPolicy, SqliteTransferStore, StoreError, TransferDirection,
    TransferEndpoint, TransferFeatureSet, TransferId, TransferQueue, TransferState, TransferStore,
    TransferTask,
};
use rusqlite::{Connection, params};
use tempfile::tempdir;
use uuid::Uuid;

fn task(id: u128, created_at_unix_ms: i64) -> TransferTask {
    TransferTask::new(
        TransferId::from_uuid(Uuid::from_u128(id)),
        TransferEndpoint::Local {
            handle: LocalFileHandle::from_uuid(Uuid::from_u128(id + 100)),
        },
        TransferEndpoint::Remote(RemoteTransferEndpoint {
            profile_id: ProfileId::from_uuid(Uuid::from_u128(id + 200)),
            protocol: RemoteProtocol::Sftp,
            path: RemotePath::new("/upload.bin").expect("remote path"),
        }),
        TransferDirection::Upload,
        None,
        None,
        RetryPolicy::default(),
        BandwidthLimit::unlimited(),
        ConflictPolicy::Fail,
        TransferFeatureSet {
            pause: FeatureSupport::Unsupported(
                SafeReason::new("fixture_resume_unavailable").expect("reason"),
            ),
            resume: FeatureSupport::Unsupported(
                SafeReason::new("fixture_resume_unavailable").expect("reason"),
            ),
            resume_validation: None,
        },
        created_at_unix_ms,
    )
    .expect("task")
}

#[test]
fn sqlite_store_is_durable_and_distinguishes_conflict_from_missing() {
    let directory = tempdir().expect("directory");
    let path = directory.path().join("transfers.sqlite3");
    let original = task(1, 10);

    {
        let mut store = SqliteTransferStore::open(&path).expect("open");
        store.insert(&original).expect("insert");
        assert_eq!(store.insert(&original), Err(StoreError::AlreadyExists));

        let mut changed = original.clone();
        changed.request_cancel(11).expect("cancel");
        store.compare_and_swap(0, &changed).expect("cas");
        assert_eq!(
            store.compare_and_swap(0, &changed),
            Err(StoreError::RevisionConflict)
        );

        let missing = task(2, 10);
        assert_eq!(
            store.compare_and_swap(0, &missing),
            Err(StoreError::NotFound)
        );
    }

    let store = SqliteTransferStore::open(&path).expect("reopen");
    let loaded = store.load_all().expect("load");
    assert_eq!(loaded.len(), 1);
    assert!(matches!(loaded[0].state, TransferState::Cancelled { .. }));
    assert_eq!(loaded[0].revision, 1);
}

#[test]
fn queue_open_recovers_active_task_and_persists_unverified_failure() {
    let directory = tempdir().expect("directory");
    let path = directory.path().join("transfers.sqlite3");
    let mut running = task(10, 100);
    running.start(101).expect("start");
    let mut store = SqliteTransferStore::open(&path).expect("open");
    store.insert(&running).expect("insert");

    let queue = TransferQueue::open(store, QueueLimits::default(), 102).expect("queue");
    let recovered = queue.task(running.id).expect("recovered");
    assert!(matches!(recovered.state, TransferState::Failed { .. }));
    assert_eq!(recovered.revision, 2);
    drop(queue);

    let store = SqliteTransferStore::open(&path).expect("reopen");
    let persisted = store.load_all().expect("load");
    assert!(matches!(persisted[0].state, TransferState::Failed { .. }));
    assert_eq!(persisted[0].revision, 2);
}

#[test]
fn store_rejects_future_schema_and_inconsistent_document_columns() {
    let directory = tempdir().expect("directory");
    let future_path = directory.path().join("future.sqlite3");
    let connection = Connection::open(&future_path).expect("sqlite");
    connection
        .pragma_update(None, "user_version", 99_u32)
        .expect("version");
    drop(connection);
    let future_bytes = std::fs::read(&future_path).expect("future database bytes");
    assert!(matches!(
        SqliteTransferStore::open(&future_path),
        Err(StoreError::UnsupportedSchema)
    ));
    assert_eq!(
        std::fs::read(&future_path).expect("future database bytes after rejection"),
        future_bytes
    );
    assert!(!future_path.with_extension("sqlite3-wal").exists());
    assert!(!future_path.with_extension("sqlite3-shm").exists());

    let corrupt_path = directory.path().join("corrupt.sqlite3");
    let mut store = SqliteTransferStore::open(&corrupt_path).expect("open");
    let persisted = task(20, 200);
    store.insert(&persisted).expect("insert");
    drop(store);

    let connection = Connection::open(&corrupt_path).expect("sqlite");
    connection
        .execute(
            "UPDATE transfer_tasks SET revision = ?1 WHERE task_id = ?2",
            params![42_i64, persisted.id.as_uuid().to_string()],
        )
        .expect("tamper");
    drop(connection);

    let store = SqliteTransferStore::open(&corrupt_path).expect("reopen");
    assert_eq!(store.load_all(), Err(StoreError::Corrupt));
}

#[test]
fn failed_initial_schema_transaction_preserves_the_version_zero_database() {
    let directory = tempdir().expect("directory");
    let path = directory.path().join("failed-initialization.sqlite3");
    let connection = Connection::open(&path).expect("sqlite");
    connection
        .execute_batch(
            "CREATE TABLE transfer_tasks (sentinel INTEGER NOT NULL) STRICT;
             INSERT INTO transfer_tasks (sentinel) VALUES (42);",
        )
        .expect("incompatible version zero fixture");
    drop(connection);

    assert!(matches!(
        SqliteTransferStore::open(&path),
        Err(StoreError::Unavailable)
    ));

    let connection = Connection::open(&path).expect("reopen fixture");
    let version: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("schema version");
    let sentinel: i64 = connection
        .query_row("SELECT sentinel FROM transfer_tasks", [], |row| row.get(0))
        .expect("sentinel row");
    let index_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'index' AND name = 'transfer_tasks_state_updated_idx'",
            [],
            |row| row.get(0),
        )
        .expect("index count");
    assert_eq!(version, 0);
    assert_eq!(sentinel, 42);
    assert_eq!(index_count, 0);
}

#[test]
fn corrupt_database_is_rejected_without_rebuild_or_sidecars() {
    let directory = tempdir().expect("directory");
    let path = directory.path().join("corrupt-file.sqlite3");
    let before = b"not a sqlite database";
    std::fs::write(&path, before).expect("corrupt fixture");

    assert!(matches!(
        SqliteTransferStore::open(&path),
        Err(StoreError::Corrupt)
    ));
    assert_eq!(std::fs::read(&path).expect("fixture bytes"), before);
    assert!(!path.with_extension("sqlite3-wal").exists());
    assert!(!path.with_extension("sqlite3-shm").exists());
}
